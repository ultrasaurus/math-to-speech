//! LaTeX math -> spoken English text, for TTS of mathematical documents.
//!
//! `mitex_parser` turns LaTeX math source into an AST (see its `syntax`
//! module); this crate walks that AST and emits the phrase a person would
//! say aloud for each construct, rather than reading the LaTeX symbols
//! literally. It covers the same LaTeX math subset `mitex` targets, plus a
//! fixed vocabulary of common symbols/Greek letters; anything else surfaces
//! as an error rather than being mis-spoken.

use anyhow::{bail, Result};
use mitex_parser::syntax::SyntaxKind::*;
use mitex_parser::syntax::SyntaxNode;
use mitex_spec_gen::DEFAULT_SPEC;
use rowan::NodeOrToken;

/// Convert LaTeX math source (no surrounding `$`/`\(`/`\[` delimiters) into
/// spoken English text.
pub fn speak(tex: &str) -> Result<String> {
    let root = mitex_parser::parse(tex, DEFAULT_SPEC.clone());
    let mut out = String::new();
    speak_children(&root, &mut out)?;
    Ok(collapse_whitespace(&out))
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn push_word(out: &mut String, word: &str) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(word);
}

/// The base and each attached sub/superscript of an `ItemAttachComponent`.
/// `base` is the node this attachment applies to; `sub`/`sup` are the
/// scripted content (everything after the `_`/`^` token), phrased as
/// "sub X" / "to the X" by the caller.
type Element = NodeOrToken<SyntaxNode, mitex_parser::syntax::SyntaxToken>;

fn speak_children(node: &SyntaxNode, out: &mut String) -> Result<()> {
    let elements: Vec<Element> = node.children_with_tokens().collect();
    speak_sequence(&elements, out)
}

/// Walks one flat sibling list (a node's direct children), phrasing
/// `(...)`/`[...]` as function application when something was just spoken
/// immediately before the bracket — `x(t)` -> "x of t", `x[n]` -> "x at
/// index n" (kept distinct from `(...)`  so discrete- and continuous-time
/// signal notation don't collapse to the same phrase) — or as silent
/// grouping otherwise, e.g. `[a, b]` as an interval, `(a+b)*c`. `(`/`[`
/// aren't grouped into their own AST node by `mitex_parser` — they're
/// plain sibling tokens next to whatever's inside them — so this scan
/// tracks bracket depth itself to find each matching close.
fn speak_sequence(elements: &[Element], out: &mut String) -> Result<()> {
    let mut has_content = false;
    let mut i = 0;
    while i < elements.len() {
        let bracket = match &elements[i] {
            NodeOrToken::Token(t) if t.kind() == TokenLParen => Some((TokenLParen, TokenRParen, "of")),
            NodeOrToken::Token(t) if t.kind() == TokenLBracket => Some((TokenLBracket, TokenRBracket, "at index")),
            _ => None,
        };
        if let Some((open_kind, close_kind, word)) = bracket {
            let mut depth = 1;
            let mut j = i + 1;
            while j < elements.len() {
                if let NodeOrToken::Token(t) = &elements[j] {
                    if t.kind() == open_kind {
                        depth += 1;
                    } else if t.kind() == close_kind {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                j += 1;
            }
            if j >= elements.len() {
                bail!("unmatched bracket in math expression");
            }
            if has_content {
                push_word(out, word);
            }
            speak_sequence(&elements[i + 1..j], out)?;
            has_content = true;
            i = j + 1;
            continue;
        }

        // `\left(...\right)` / `\left[...\right]` — mitex_parser groups
        // these into one `ItemLR` node (unlike bare `(`/`[`, which are
        // flat sibling tokens, handled above), but the same "of"/"at
        // index"/plain-grouping decision applies based on the opening
        // delimiter and whatever preceded it.
        if let NodeOrToken::Node(node) = &elements[i] {
            if node.kind() == ItemLR {
                let (word, inner) = left_right_group(node)?;
                if has_content {
                    if let Some(word) = word {
                        push_word(out, word);
                    }
                }
                speak_sequence(&inner, out)?;
                has_content = true;
                i += 1;
                continue;
            }
        }

        speak_element(&elements[i], out)?;
        if !matches!(&elements[i], NodeOrToken::Token(t) if matches!(t.kind(), TokenWhiteSpace | TokenLineBreak | TokenComment)) {
            has_content = true;
        }
        i += 1;
    }
    Ok(())
}

/// Splits an `ItemLR` node (`\left DELIM ... \right DELIM`) into the word
/// to speak for its opening delimiter (`None` for `\left\{`/`\left.`/etc.
/// — brace and "invisible" delimiters are plain grouping, no "of"/"at
/// index" trigger word, same as a bare `{...}` group) and its middle
/// content, as an element list ready for `speak_sequence`.
fn left_right_group(node: &SyntaxNode) -> Result<(Option<&'static str>, Vec<Element>)> {
    let children: Vec<Element> = node.children_with_tokens().collect();
    let clause_positions: Vec<usize> =
        children.iter().enumerate().filter(|(_, e)| matches!(e, NodeOrToken::Node(n) if n.kind() == ClauseLR)).map(|(i, _)| i).collect();
    let [open_pos, close_pos] = clause_positions.as_slice() else {
        bail!("\\left...\\right group with {} clauses, expected 2", clause_positions.len());
    };

    let NodeOrToken::Node(open_clause) = &children[*open_pos] else { unreachable!() };
    let word = open_clause
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find_map(|t| match t.kind() {
            TokenLParen => Some("of"),
            TokenLBracket => Some("at index"),
            _ => None,
        });

    let inner = children[open_pos + 1..*close_pos].to_vec();
    Ok((word, inner))
}

/// `-`/`=`/`<`/`>` glued directly into a word (no surrounding spaces) —
/// `mitex` lexes `N-1`, `x=y`, `t<0`, and even a leading `-1` as one
/// `TokenWord` each, so there's no separate token to catch these
/// operators generically the way a *spaced* `=` or `<` is (a bare
/// standalone token, handled by `speak_element`'s literal `TokenWord`
/// branch calling this same function). `None` if `word` has none of these
/// characters at all (the common case, left to the caller's literal
/// pass-through). Within math mode specifically (unlike general prose) an
/// embedded `-` essentially never means a hyphenated compound word, so
/// it's safe to always read it as arithmetic: a leading `-` ("-1") is a
/// negative number, an internal one ("N-1") is subtraction; `=`/`<`/`>`
/// are always spelled as words regardless of position — left as the
/// literal character, some TTS engines (confirmed: misaki/espeak, and the
/// vibe/F5 model) silently produce no phonemes for them at all, dropping
/// them from the audio rather than mispronouncing them.
fn speak_operator_word(word: &str) -> Option<String> {
    if !word.contains(['-', '=', '<', '>']) {
        return None;
    }
    let chars: Vec<char> = word.chars().collect();
    let mut phrase = String::new();
    let mut segment_start = 0;
    let mut wrote_any = false;

    let push = |phrase: &mut String, wrote_any: &mut bool, text: &str| {
        if text.is_empty() {
            return;
        }
        if *wrote_any {
            phrase.push(' ');
        }
        phrase.push_str(text);
        *wrote_any = true;
    };

    for (i, &c) in chars.iter().enumerate() {
        let op_word = match c {
            '-' if i == 0 => Some("negative"),
            '-' => Some("minus"),
            '=' => Some("equals"),
            '<' => Some("less than"),
            '>' => Some("greater than"),
            _ => None,
        };
        let Some(op_word) = op_word else { continue };
        let segment: String = chars[segment_start..i].iter().collect();
        push(&mut phrase, &mut wrote_any, &segment);
        push(&mut phrase, &mut wrote_any, op_word);
        segment_start = i + 1;
    }
    let tail: String = chars[segment_start..].iter().collect();
    push(&mut phrase, &mut wrote_any, &tail);
    Some(phrase)
}

fn speak_element(element: &Element, out: &mut String) -> Result<()> {
    match element {
        NodeOrToken::Node(node) => speak_node(node, out),
        NodeOrToken::Token(tok) => match tok.kind() {
            TokenWhiteSpace | TokenLineBreak | TokenComment => Ok(()),
            TokenWord => {
                if let Some(phrase) = speak_operator_word(tok.text()) {
                    push_word(out, &phrase);
                } else {
                    push_word(out, tok.text());
                }
                Ok(())
            }
            TokenComma => {
                out.push(',');
                Ok(())
            }
            TokenAsterisk => {
                push_word(out, "asterisk");
                Ok(())
            }
            TokenSlash => {
                // Same word `\frac{a}{b}` already produces ("a over b"),
                // so `a / b` and `\frac{a}{b}` read identically regardless
                // of which way an author wrote the fraction.
                push_word(out, "over");
                Ok(())
            }
            TokenLBrace | TokenRBrace => Ok(()),
            other => bail!("unsupported token in math expression: {other:?} ({:?})", tok.text()),
        },
    }
}

fn speak_node(node: &SyntaxNode, out: &mut String) -> Result<()> {
    match node.kind() {
        ScopeRoot | ItemFormula => speak_children(node, out),
        ItemCurly => speak_children(node, out),
        ItemText => speak_children(node, out),
        ItemCmd => speak_cmd(node, out),
        ItemAttachComponent => speak_attach(node, out),
        other => bail!("unsupported math construct: {other:?}"),
    }
}

struct Attach {
    base: SyntaxNode,
    sub: Option<Vec<Element>>,
    sup: Option<Vec<Element>>,
}

fn speak_attach(node: &SyntaxNode, out: &mut String) -> Result<()> {
    let attach = parse_attach(node)?;
    speak_children(&attach.base, out)?;
    if let Some(sup) = &attach.sup {
        if let Some(suffix) = ordinal_suffix(sup) {
            // No `push_word` here — an ordinal suffix attaches directly to
            // the base with no space: "n" + "th" -> "nth", not "n th".
            out.push_str(suffix);
        } else if let Some(power) = simple_power_word(sup) {
            push_word(out, power);
        } else if is_degree_symbol(sup) {
            push_word(out, "degrees");
        } else {
            push_word(out, "to the");
            speak_sequence(sup, out)?;
        }
    }
    if let Some(sub) = &attach.sub {
        push_word(out, "sub");
        speak_sequence(sub, out)?;
    }
    Ok(())
}

/// "th"/"st"/"nd"/"rd" for a braced ordinal superscript — `x^{th}` or
/// `x^\text{th}` (both common; authors reach for `\text{}` specifically to
/// keep the suffix upright/non-italic in rendered math, so both must
/// resolve the same way) — `None` otherwise. Written with braces, not
/// `x^th`: LaTeX applies an unbraced superscript to only the single next
/// character, so `x^th` parses as `x^t` followed by a plain trailing "h",
/// not this case.
fn ordinal_suffix(sup: &[Element]) -> Option<&'static str> {
    let [Element::Node(node)] = sup else { return None };
    let curly = unwrap_text_command(node)?;
    if curly.kind() != ItemCurly {
        return None;
    }
    let inner: Vec<Element> = curly
        .children_with_tokens()
        .filter(|e| !matches!(e, NodeOrToken::Token(t) if t.kind() == TokenLBrace || t.kind() == TokenRBrace))
        .collect();
    let [Element::Node(text)] = inner.as_slice() else { return None };
    if text.kind() != ItemText {
        return None;
    }
    let mut toks = text.children_with_tokens().filter_map(|e| e.into_token());
    let only = toks.next()?;
    if toks.next().is_some() || only.kind() != TokenWord {
        return None;
    }
    match only.text() {
        "st" => Some("st"),
        "nd" => Some("nd"),
        "rd" => Some("rd"),
        "th" => Some("th"),
        _ => None,
    }
}

/// `true` for a bare `\circ` superscript (`360^\circ`) — the standard way
/// to write a degree symbol in LaTeX math. Kept separate from
/// `ordinal_suffix`/`simple_power_word`: "degrees" is a full word attached
/// with a space ("360 degrees"), not concatenated like an ordinal suffix
/// ("nth") or a single power word.
fn is_degree_symbol(sup: &[Element]) -> bool {
    let [Element::Node(cmd)] = sup else { return false };
    if cmd.kind() != ItemCmd {
        return false;
    }
    let Some(name_tok) = cmd
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == ClauseCommandName)
    else {
        return false;
    };
    name_tok.text().trim_start_matches('\\') == "circ"
}

/// `\text{...}` wrapping a single braced argument — unwraps it to that
/// `ItemCurly` node, so a superscript shape check can treat `x^\text{th}`
/// the same as `x^{th}`. Returns `node` itself unchanged if it isn't a
/// `\text` command (so a bare `ItemCurly` passes straight through).
fn unwrap_text_command(node: &SyntaxNode) -> Option<SyntaxNode> {
    if node.kind() != ItemCmd {
        return Some(node.clone());
    }
    let name_tok = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == ClauseCommandName)?;
    if name_tok.text().trim_start_matches('\\') != "text" {
        return None;
    }
    let arg = node.children().find(|n| n.kind() == ClauseArgument)?;
    let children: Vec<Element> = arg.children_with_tokens().collect();
    let [Element::Node(curly)] = children.as_slice() else {
        return None;
    };
    Some(curly.clone())
}

/// "squared" / "cubed" for a bare `^2` / `^3` superscript; `None` for
/// anything else, so the caller falls back to "to the N".
fn simple_power_word(sup: &[Element]) -> Option<&'static str> {
    let [Element::Token(only)] = sup else {
        return None;
    };
    match only.text() {
        "2" => Some("squared"),
        "3" => Some("cubed"),
        _ => None,
    }
}

/// `ItemAttachComponent` nests: `x_1^2` is an attach-of-an-attach, with the
/// innermost holding `x`. Each level's `ClauseArgument` child wraps the
/// base (itself possibly another `ItemAttachComponent`); everything after
/// the `_`/`^` token is that level's scripted content. Walk down to the
/// real base, collecting sub/superscript content from each level.
fn parse_attach(node: &SyntaxNode) -> Result<Attach> {
    let mut base_node = node.clone();
    let mut sub = None;
    let mut sup = None;
    loop {
        let elements: Vec<Element> = base_node.children_with_tokens().collect();
        let Some(NodeOrToken::Node(arg)) = elements.first() else {
            bail!("attachment with no base");
        };
        let arg = arg.clone();

        let script_start = elements
            .iter()
            .position(|e| matches!(e, NodeOrToken::Token(t) if t.kind() == TokenUnderscore || t.kind() == TokenCaret))
            .ok_or_else(|| anyhow::anyhow!("attachment with no _ or ^ token"))?;
        let is_sub = matches!(&elements[script_start], NodeOrToken::Token(t) if t.kind() == TokenUnderscore);
        let script_content: Vec<Element> = elements[script_start + 1..].to_vec();

        if is_sub {
            sub.get_or_insert(script_content);
        } else {
            sup.get_or_insert(script_content);
        }

        let inner = arg.children().next();
        match inner {
            Some(inner) if inner.kind() == ItemAttachComponent => {
                base_node = inner;
                continue;
            }
            _ => return Ok(Attach { base: arg, sub, sup }),
        }
    }
}

fn speak_cmd(node: &SyntaxNode, out: &mut String) -> Result<()> {
    let name_tok = node
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == ClauseCommandName);
    let Some(name_tok) = name_tok else {
        bail!("command with no name");
    };
    let name = name_tok.text().trim_start_matches('\\');

    let args: Vec<SyntaxNode> = node
        .children()
        .filter(|n| n.kind() == ClauseArgument)
        .collect();

    match name {
        "frac" => {
            let [num, den] = require_args(&args, "frac")?;
            speak_children(num, out)?;
            push_word(out, "over");
            speak_children(den, out)?;
            Ok(())
        }
        "sqrt" => {
            match args.len() {
                1 => {
                    push_word(out, "the square root of");
                    speak_children(&args[0], out)?;
                }
                2 => {
                    push_word(out, "the");
                    speak_children(&args[0], out)?;
                    push_word(out, "root of");
                    speak_children(&args[1], out)?;
                }
                n => bail!("sqrt with {n} arguments"),
            }
            Ok(())
        }
        "text" | "mathrm" => match args.as_slice() {
            [content] => speak_children(content, out),
            other => bail!("\\{name} expects 1 argument, found {}", other.len()),
        },
        // `\red`/`\blue` (bare color shorthand) aren't registered with an
        // argument in `mitex_parser`'s default command spec, unlike
        // `\textcolor{color}{body}` below — they parse as a bare 0-arg
        // command with the following `{X}` as a separate sibling group,
        // not `\red`'s `ClauseArgument`. So there's nothing to do here but
        // contribute no words; that sibling `{X}` group already speaks its
        // own content normally via the generic `ItemCurly` handling in
        // `speak_node`, which is exactly "keep the content, drop the
        // color" — color is a visual cue with no bearing on how the math
        // reads aloud.
        "red" | "blue" => Ok(()),
        "textcolor" => match args.as_slice() {
            // First argument is the color name/spec — ignored, same reason
            // as `\red`/`\blue` above.
            [_color, content] => speak_children(content, out),
            other => bail!("\\textcolor expects 2 arguments, found {}", other.len()),
        },
        "cancel" | "xcancel" | "bcancel" => match args.as_slice() {
            // Canceled-out content is visually struck through specifically
            // to mark it as removed from the expression — speaking it
            // would contradict that, so it's silently dropped rather than
            // read aloud.
            [_content] => Ok(()),
            other => bail!("\\{name} expects 1 argument, found {}", other.len()),
        },
        _ => {
            if let Some(word) = symbol_word(name) {
                push_word(out, word);
                Ok(())
            } else {
                bail!("unsupported command: \\{name}")
            }
        }
    }
}

fn require_args<'a>(args: &'a [SyntaxNode], name: &str) -> Result<[&'a SyntaxNode; 2]> {
    match args {
        [a, b] => Ok([a, b]),
        other => bail!("\\{name} expects 2 arguments, found {}", other.len()),
    }
}

/// Fixed vocabulary for symbols/Greek letters with no arguments. Anything
/// not listed here is rejected rather than guessed.
fn symbol_word(name: &str) -> Option<&'static str> {
    Some(match name {
        "alpha" => "alpha",
        "beta" => "beta",
        "gamma" => "gamma",
        "delta" => "delta",
        "epsilon" | "varepsilon" => "epsilon",
        "zeta" => "zeta",
        "eta" => "eta",
        "theta" => "theta",
        "iota" => "iota",
        "kappa" => "kappa",
        "lambda" => "lambda",
        "mu" => "mu",
        "nu" => "nu",
        "xi" => "xi",
        "pi" => "pi",
        "rho" => "rho",
        "sigma" => "sigma",
        "tau" => "tau",
        "upsilon" => "upsilon",
        "phi" | "varphi" => "phi",
        "chi" => "chi",
        "psi" => "psi",
        "omega" => "omega",
        "infty" => "infinity",
        "partial" => "partial",
        "leq" | "le" => "less than or equal to",
        "geq" | "ge" => "greater than or equal to",
        "neq" | "ne" => "not equal to",
        "approx" => "approximately",
        "propto" => "is proportional to",
        "times" => "times",
        "cdot" => "times",
        "pm" => "plus or minus",
        "sum" => "the sum of",
        "prod" => "the product of",
        "int" => "the integral of",
        // Named functions: bare here, "of" comes from `speak_sequence`
        // seeing the `(...)` that follows, e.g. `\sin(x)` -> "sine of x".
        "sin" => "sine",
        "cos" => "cosine",
        "tan" => "tangent",
        "cot" => "cotangent",
        "sec" => "secant",
        "csc" => "cosecant",
        "arcsin" => "arc sine",
        "arccos" => "arc cosine",
        "arctan" => "arc tangent",
        "sinh" => "hyperbolic sine",
        "cosh" => "hyperbolic cosine",
        "tanh" => "hyperbolic tangent",
        "log" => "log",
        "ln" => "natural log",
        "exp" => "the exponential function",
        "lim" => "the limit of",
        "min" => "the minimum of",
        "max" => "the maximum of",
        "det" => "the determinant of",
        "gcd" => "the greatest common divisor of",
        "dots" | "ldots" | "cdots" | "vdots" | "ddots" => "dot dot dot",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::speak;

    #[test]
    fn fraction() {
        assert_eq!(speak(r"\frac{\pi}{2}").unwrap(), "pi over 2");
    }

    #[test]
    fn squared() {
        assert_eq!(speak("x^2").unwrap(), "x squared");
    }

    #[test]
    fn general_power() {
        assert_eq!(speak("x^n").unwrap(), "x to the n");
    }

    #[test]
    fn subscript() {
        assert_eq!(speak("x_i").unwrap(), "x sub i");
    }

    #[test]
    fn sub_and_sup() {
        assert_eq!(speak("x_i^2").unwrap(), "x squared sub i");
    }

    #[test]
    fn sqrt_plain() {
        assert_eq!(speak(r"\sqrt{2}").unwrap(), "the square root of 2");
    }

    #[test]
    fn greek_and_symbol() {
        assert_eq!(speak(r"\alpha \leq \beta").unwrap(), "alpha less than or equal to beta");
    }

    #[test]
    fn text_passthrough() {
        assert_eq!(speak(r"\text{RMS}").unwrap(), "RMS");
    }

    #[test]
    fn unsupported_rejected() {
        assert!(speak(r"\begin{matrix}1&2\end{matrix}").is_err());
    }

    #[test]
    fn continuous_time_signal() {
        assert_eq!(speak("x(t)").unwrap(), "x of t");
    }

    #[test]
    fn discrete_time_signal() {
        assert_eq!(speak("x[n]").unwrap(), "x at index n");
    }

    #[test]
    fn named_function_with_parens() {
        assert_eq!(speak(r"\sin(x)").unwrap(), "sine of x");
        assert_eq!(speak(r"\cos(\omega t)").unwrap(), "cosine of omega t");
    }

    #[test]
    fn nested_function_application() {
        assert_eq!(speak(r"\sin(\cos(x))").unwrap(), "sine of cosine of x");
    }

    #[test]
    fn interval_notation_stays_plain_grouping() {
        assert_eq!(speak("[a, b]").unwrap(), "a, b");
    }

    #[test]
    fn plain_grouping_parens_no_of() {
        assert_eq!(speak("(a+b)").unwrap(), "a+b");
    }

    #[test]
    fn ordinal_superscript() {
        assert_eq!(speak("n^{th}").unwrap(), "nth");
        assert_eq!(speak("1^{st}").unwrap(), "1st");
        assert_eq!(speak("2^{nd}").unwrap(), "2nd");
        assert_eq!(speak("3^{rd}").unwrap(), "3rd");
    }

    // `\text{th}` (not just bare `{th}`) is a common way authors write an
    // ordinal suffix, to keep it upright/non-italic in rendered math.
    #[test]
    fn ordinal_superscript_wrapped_in_text_command() {
        assert_eq!(speak(r"n^\text{th}").unwrap(), "nth");
        assert_eq!(speak(r"1^\text{st}").unwrap(), "1st");
    }

    #[test]
    fn degree_symbol() {
        assert_eq!(speak(r"360^\circ").unwrap(), "360 degrees");
    }

    #[test]
    fn cancel_content_is_silent() {
        assert_eq!(speak(r"\cancel{5} \cdot 3").unwrap(), "times 3");
        assert_eq!(speak(r"\xcancel{5}").unwrap(), "");
        assert_eq!(speak(r"\bcancel{5}").unwrap(), "");
    }

    #[test]
    fn color_commands_speak_only_their_content() {
        assert_eq!(speak(r"\red{x} + \blue{y}").unwrap(), "x + y");
        assert_eq!(speak(r"\textcolor{red}{x} + y").unwrap(), "x + y");
    }

    // The motivating real-world case: colored, canceled units in a
    // unit-conversion derivation should read as if the canceled parts and
    // color simply weren't there.
    #[test]
    fn colored_cancel_content_is_silent() {
        assert_eq!(speak(r"\red{\cancel{\text{cycle}}}").unwrap(), "");
    }

    #[test]
    fn hyphenated_negative_and_subtraction() {
        assert_eq!(speak("-1").unwrap(), "negative 1");
        assert_eq!(speak("N-1").unwrap(), "N minus 1");
        assert_eq!(speak("5-3").unwrap(), "5 minus 3");
        assert_eq!(speak(r"0, 1, 2, \dots, N-1").unwrap(), "0, 1, 2, dot dot dot, N minus 1");
    }

    #[test]
    fn comparison_operators_spoken_spaced_or_glued() {
        assert_eq!(speak("t < 0").unwrap(), "t less than 0");
        assert_eq!(speak("t<0").unwrap(), "t less than 0");
        assert_eq!(speak("t > 0").unwrap(), "t greater than 0");
        assert_eq!(speak("t>0").unwrap(), "t greater than 0");
    }

    #[test]
    fn equals_sign_spoken_spaced_or_glued() {
        assert_eq!(speak("x = y").unwrap(), "x equals y");
        assert_eq!(speak("x=y").unwrap(), "x equals y");
    }

    #[test]
    fn bare_asterisk() {
        assert_eq!(speak("*").unwrap(), "asterisk");
    }

    #[test]
    fn slash_division() {
        assert_eq!(speak("5 / C").unwrap(), "5 over C");
    }

    #[test]
    fn left_right_delimiters() {
        // The motivating real-world case: units in brackets.
        assert_eq!(speak(r"\left[\frac{\text{W}}{\text{m}^2}\right]").unwrap(), "W over m squared");
        // Same function-application/grouping rules as bare `(`/`[` apply.
        assert_eq!(speak(r"x\left(t\right)").unwrap(), "x of t");
        assert_eq!(speak(r"\left(a+b\right)").unwrap(), "a+b");
        assert_eq!(speak(r"\left[a, b\right]").unwrap(), "a, b");
    }

    #[test]
    fn proportional_to() {
        assert_eq!(speak(r"I \propto p^2").unwrap(), "I is proportional to p squared");
    }

    #[test]
    fn bare_equals_sign_spoken_as_word() {
        assert_eq!(speak("y = g(x)").unwrap(), "y equals g of x");
    }

    #[test]
    fn ellipsis_commands() {
        assert_eq!(speak(r"0, 1, 2, \dots").unwrap(), "0, 1, 2, dot dot dot");
        assert_eq!(speak(r"\cdots").unwrap(), "dot dot dot");
    }

    // Unbraced `n^th` is standard LaTeX for `n^t` followed by a plain
    // trailing "h" — a superscript with no braces only ever applies to the
    // single next character. Not this crate's call to special-case.
    #[test]
    fn unbraced_ordinal_only_takes_one_character() {
        assert_eq!(speak("n^th").unwrap(), "n to the t h");
    }
}
