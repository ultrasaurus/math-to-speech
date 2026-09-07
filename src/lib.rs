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

/// Speaks `node`'s direct children as a fresh sequence — used for every
/// subexpression that starts its own "has anything been spoken yet"
/// context (a `{...}` group, a command argument, environment content,
/// etc.). The one context that must *not* start fresh is an attach node's
/// base (see `speak_attach`), which inherits the enclosing sequence's
/// `has_content` directly via `speak_sequence` instead of going through
/// this wrapper.
fn speak_children(node: &SyntaxNode, out: &mut String) -> Result<()> {
    let elements: Vec<Element> = node.children_with_tokens().collect();
    let mut has_content = false;
    speak_sequence(&elements, out, &mut has_content)
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
///
/// `has_content` is whether anything has already been spoken in *this*
/// sequence — normally starts `false` (via `speak_children`), but an
/// attach node's base inherits the caller's current value instead (see
/// `speak_attach`), since mitex can glue a mid-sequence `-` onto the next
/// operand's leading token (`p_1-p_2` parses `-p` as one word, the base of
/// the second `_2` attach) and word-local position alone can't tell that
/// apart from a truly leading `-`.
fn speak_sequence(elements: &[Element], out: &mut String, has_content: &mut bool) -> Result<()> {
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
                if let Some(kind) = element_bracket_kind(&elements[j]) {
                    if kind == open_kind {
                        depth += 1;
                    } else if kind == close_kind {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                }
                j += 1;
            }
            if j < elements.len() {
                // See the `ItemLR` handling in this same function for why
                // a connective word immediately before the bracket (`x =
                // (a+b)`, `V \cdot (...)`) must not trigger the `x(t)`
                // "of"/"at index" function-application wording.
                let prev_is_connective = prev_nontrivial_index(elements, i)
                    .and_then(|k| render_element_alone(&elements[k]).ok())
                    .is_some_and(|phrase| is_connective_word(&phrase));
                if *has_content && !prev_is_connective {
                    push_word(out, word);
                }
                let mut inner_has_content = false;
                speak_sequence(&elements[i + 1..j], out, &mut inner_has_content)?;
                if let NodeOrToken::Node(n) = &elements[j] {
                    speak_attach_scripts(&parse_attach(n)?, out)?;
                }
                *has_content = true;
                i = j + 1;
                continue;
            }

            // No same-kind close found — try interval notation, where the
            // closing delimiter is deliberately the *other* bracket kind
            // (`[a, b)` for a half-open interval). Track combined depth
            // across both bracket families to find the real close, then
            // require a top-level comma splitting the two bounds; anything
            // short of that isn't an interval, just malformed math.
            if let Some((close_kind, j)) = find_interval_close(&elements[i + 1..]).map(|(k, j)| (k, j + i + 1)) {
                // `mitex_parser` groups a run of words/commas/whitespace
                // into one `ItemText` node (`a, b` -> a single `ItemText`,
                // not flat sibling tokens) — flatten that back out so the
                // comma between the two bounds is visible to
                // `top_level_comma`.
                let inner = flatten_text_nodes(&elements[i + 1..j]);
                let Some(comma_pos) = top_level_comma(&inner) else {
                    bail!("unmatched bracket in math expression");
                };
                push_word(out, "the interval from");
                let mut lower_has_content = false;
                speak_sequence(&inner[..comma_pos], out, &mut lower_has_content)?;
                push_word(out, if open_kind == TokenLBracket { "inclusive," } else { "exclusive," });
                push_word(out, "to");
                let mut upper_has_content = false;
                speak_sequence(&inner[comma_pos + 1..], out, &mut upper_has_content)?;
                push_word(out, if close_kind == TokenRBracket { "inclusive" } else { "exclusive" });
                *has_content = true;
                i = j + 1;
                continue;
            }

            bail!("unmatched bracket in math expression");
        }

        // `\left(...\right)` / `\left[...\right]` / `\left|...\right|` —
        // mitex_parser groups these into one `ItemLR` node (unlike bare
        // `(`/`[`, which are flat sibling tokens, handled above); see
        // `LRPrefix`/`left_right_group` for the phrasing decision based on
        // the opening delimiter and whatever preceded it.
        if let NodeOrToken::Node(node) = &elements[i] {
            if node.kind() == ItemLR {
                let (prefix, inner) = left_right_group(node)?;
                // A connective word (`\cdot`, `+`, `=`, `\leq`, ...) spoken
                // immediately before this group doesn't count as "an
                // operand was just spoken" for `IfPreceded` purposes — only
                // an actual value/expression/function name does. Without
                // this, `V \cdot (\frac{a}{b})` reads as "V times of a over
                // b": `\cdot` already set `has_content`, so the grouping
                // paren wrongly took the `x(t)` "of" phrasing meant for
                // function application, not "times" followed by a plain
                // grouped multiplicand.
                let prev_is_connective = prev_nontrivial_index(elements, i)
                    .and_then(|j| render_element_alone(&elements[j]).ok())
                    .is_some_and(|phrase| is_connective_word(&phrase));
                match prefix {
                    LRPrefix::None => {}
                    LRPrefix::IfPreceded(word) => {
                        if *has_content && !prev_is_connective {
                            push_word(out, word);
                        }
                    }
                    LRPrefix::Always(word) => push_word(out, word),
                }
                let mut inner_has_content = false;
                speak_sequence(&inner, out, &mut inner_has_content)?;
                *has_content = true;
                i += 1;
                continue;
            }
        }

        // A trailing `+` right before a row break (`... + \\`, continuing
        // a sum onto the next row) grammatically belongs to the *next*
        // row's clause, not the one ending — "phi sub 1; plus, A sub 2..."
        // reads correctly, "phi sub 1 plus; A sub 2..." doesn't (the
        // semicolon lands mid-clause, right after the connective word that
        // should introduce what follows it). So the row-break semicolon
        // is emitted *before* this trailing `+`, and the `+` itself is
        // followed by a comma rather than running straight into the next
        // row's content.
        if is_lone_plus(&elements[i]) {
            if let Some(nl_idx) = next_nontrivial_index(elements, i) {
                if matches!(&elements[nl_idx], NodeOrToken::Token(t) if t.kind() == ItemNewLine) {
                    out.push(';');
                    push_word(out, "plus");
                    out.push(',');
                    *has_content = true;
                    i = nl_idx + 1;
                    continue;
                }
            }
        }

        // Bare `a / b` division where `a` and `b` are the single elements
        // immediately either side of the slash (mirrors the `\frac{a}{b}`
        // "itself" collapse — see there for rationale). Deliberately
        // narrow: only the two elements directly adjacent to the slash
        // (skipping whitespace) are compared, not a wider expression, so
        // this can't misfire on something like `a - b / a + b` by
        // comparing the whole `a - b` / `a + b` sides.
        if let NodeOrToken::Token(t) = &elements[i] {
            if t.kind() == TokenSlash {
                if let (Some(prev_idx), Some(next_idx)) =
                    (prev_nontrivial_index(elements, i), next_nontrivial_index(elements, i))
                {
                    let prev_phrase = render_element_alone(&elements[prev_idx])?;
                    let next_phrase = render_element_alone(&elements[next_idx])?;
                    if !prev_phrase.is_empty() && prev_phrase == next_phrase {
                        push_word(out, "over");
                        push_word(out, "itself");
                        *has_content = true;
                        i = next_idx + 1;
                        continue;
                    }
                    // A small named fraction (`3 / 2` -> "three halves") —
                    // see `named_fraction_word`'s doc comment. `prev_phrase`
                    // is already sitting in `out` from the previous loop
                    // iteration, so only the denominator's word is pushed
                    // here, in place of "over" + the denominator itself.
                    if let Some(word) = named_fraction_word(&prev_phrase, &next_phrase) {
                        push_word(out, word);
                        *has_content = true;
                        i = next_idx + 1;
                        continue;
                    }
                }
            }
        }

        // `\cdot` between two "atomic" factors (a number, a plain or
        // subscripted variable, `\pi`, a named function) is silent, the
        // same way a person reads `A_1 \cdot \cos(2\pi \cdot f_1 \cdot t)`
        // aloud as "A one, cosine of two pi, f one, t" rather than spelling
        // out every implicit multiplication as "times". `\times` is left
        // alone — unlike `\cdot`, it's normally chosen specifically to
        // call out multiplication rather than glue adjacent factors, e.g.
        // `3 \times 4`. Only suppressed when *both* sides are atomic, so
        // e.g. `(a+b) \cdot (c+d)` or `\sqrt{2} \cdot 3` keep "times" —
        // dropping it there would be genuinely ambiguous.
        //
        // A comma takes "times"'s place rather than nothing at all — with
        // no separator, adjacent atomic factors run together into a single
        // unclear blob ("2 pi f sub 1 t"); the comma preserves a clear
        // prosodic break between factors without spelling out "times"
        // every time. Glued directly onto the prior word (no leading
        // space) the same way every other comma in this file is, so
        // `push_word`'s own space-before-next-word logic supplies the gap
        // before the next factor.
        if let NodeOrToken::Node(node) = &elements[i] {
            if node.kind() == ItemCmd && cmd_name(node).as_deref() == Some("cdot") {
                if let (Some(prev_idx), Some(next_idx)) =
                    (prev_nontrivial_index(elements, i), next_nontrivial_index(elements, i))
                {
                    if is_atomic_multiplicand(&elements[prev_idx], Side::Prev)
                        && is_atomic_multiplicand(&elements[next_idx], Side::Next)
                    {
                        out.push(',');
                        i += 1;
                        continue;
                    }
                }
            }
        }

        speak_element(&elements[i], out, *has_content)?;
        // Tokens that `speak_element` speaks as nothing (see its match arms
        // below) must not count as "just spoke something" either, or the
        // next real content wrongly triggers `(`/`[`'s "of"/"at index"
        // wording — e.g. `{(x-2)}`'s opening `{` was marking `has_content`
        // before the `(` it precedes, producing "of x minus 2".
        if !matches!(
            &elements[i],
            NodeOrToken::Token(t) if matches!(
                t.kind(),
                TokenWhiteSpace | TokenLineBreak | TokenComment | TokenTilde | TokenAmpersand | TokenLBrace | TokenRBrace
            )
        ) {
            *has_content = true;
        }
        i += 1;
    }
    Ok(())
}

/// The index of the nearest element before `i` that isn't whitespace,
/// a line break, or a comment — `None` if `i` is at (or past) the start
/// of `elements` with nothing else in between.
fn prev_nontrivial_index(elements: &[Element], i: usize) -> Option<usize> {
    (0..i).rev().find(|&j| !is_trivial(&elements[j]))
}

/// The index of the nearest element after `i` that isn't whitespace, a
/// line break, or a comment — `None` if nothing else follows.
fn next_nontrivial_index(elements: &[Element], i: usize) -> Option<usize> {
    (i + 1..elements.len()).find(|&j| !is_trivial(&elements[j]))
}

fn is_trivial(element: &Element) -> bool {
    matches!(element, NodeOrToken::Token(t) if matches!(t.kind(), TokenWhiteSpace | TokenLineBreak | TokenComment))
}

/// True for an element that's nothing but a standalone `+` — either a
/// bare `TokenWord` (`"+"` on its own) or an `ItemText` wrapper around
/// exactly one such token (mitex wraps a run like `b \;+` so that the `+`
/// ends up as the sole non-trivial token in an `ItemText` node, not a bare
/// sibling). Used to spot a sum continuing across a row break (see the
/// row-break handling above).
fn is_lone_plus(element: &Element) -> bool {
    let is_plus_token = |t: &mitex_parser::syntax::SyntaxToken| t.kind() == TokenWord && t.text() == "+";
    match element {
        NodeOrToken::Token(t) => is_plus_token(t),
        NodeOrToken::Node(n) if n.kind() == ItemText => {
            let toks: Vec<_> = n.children_with_tokens().filter_map(|e| e.into_token()).filter(|t| !is_trivial(&NodeOrToken::Token(t.clone()))).collect();
            matches!(toks.as_slice(), [only] if is_plus_token(only))
        }
        _ => false,
    }
}

/// True when the rendered phrase of a preceding element *ends with* a
/// binary operator/relation word (`\cdot` -> "times", a bare `+` ->
/// "plus", `\leq` -> "less than or equal to", ...) — words that connect
/// two operands rather than naming one themselves. Checked as a suffix,
/// not exact equality, because mitex groups a run like `x = ` into one
/// `ItemText` node/element (`"x equals"`), not separate `x`/`=` siblings —
/// what matters is only the trailing word(s), i.e. what was *just* spoken
/// right before the following bracket. Used to tell a genuine operand
/// from a connective when deciding whether a `(...)`/`[...]` right after
/// it means function application (see the `ItemLR` handling above).
fn is_connective_word(phrase: &str) -> bool {
    const CONNECTIVES: &[&str] = &[
        "times",
        "plus",
        "minus",
        "negative",
        "equals",
        "less than",
        "greater than",
        "less than or equal to",
        "greater than or equal to",
        "not equal to",
        "approximately",
        "is proportional to",
        "on the order of",
        "is much less than",
        "is much greater than",
        "plus or minus",
        "is equivalent to",
        "is perpendicular to",
        "is parallel to",
        "mod",
        "is an element of",
        "is not an element of",
        "such that",
        "goes to",
        "implies",
        "is implied by",
        "if and only if",
    ];
    CONNECTIVES.iter().any(|c| phrase == *c || phrase.ends_with(&format!(" {c}")))
}

/// An `ItemCmd` node's command name, without the leading `\` — `None` if
/// the node has no `ClauseCommandName` child (shouldn't happen for a
/// well-formed command, but this is also used speculatively on arbitrary
/// elements).
fn cmd_name(node: &SyntaxNode) -> Option<String> {
    node.children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == ClauseCommandName)
        .map(|t| t.text().trim_start_matches('\\').to_string())
}

/// Which side of a `\cdot` an element is on — determines which end of a
/// multi-token `ItemText` run (see below) actually borders the `\cdot`.
#[derive(Clone, Copy)]
enum Side {
    Prev,
    Next,
}

/// True for an element that reads as one self-contained lexical item —
/// a number/variable token, a subscripted/superscripted variable whose
/// base is itself atomic, or a bare symbol/named-function command — as
/// opposed to a compound expression (a parenthesized group, a sum of
/// terms, `\frac`/`\sqrt`/`\sum`-style commands that already speak as
/// their own multi-word phrase). Used to decide whether a `\cdot` between
/// two factors can be silent (see the `ItemCmd` "cdot" handling above)
/// without creating ambiguity.
///
/// `side` matters for an `ItemText` run: mitex groups something like
/// `t + \phi_1` into one `ItemText` node (`t`, `+`, ...) rather than
/// separate siblings, so only the token actually touching the `\cdot`
/// (the last one for a `Prev` element, the first for a `Next` element)
/// is relevant — the rest of the run belongs to a different operator
/// entirely and says nothing about whether *this* multiplication is
/// ambiguous.
fn is_atomic_multiplicand(element: &Element, side: Side) -> bool {
    match element {
        NodeOrToken::Token(t) => t.kind() == TokenWord,
        NodeOrToken::Node(n) if n.kind() == ItemAttachComponent => {
            let Ok(attach) = parse_attach(n) else { return false };
            let base = flat_base_elements(&attach.base);
            matches!(base.as_slice(), [only] if is_atomic_multiplicand(only, side))
        }
        NodeOrToken::Node(n) if n.kind() == ItemCmd => {
            const PHRASE_COMMANDS: &[&str] =
                &["frac", "sqrt", "sum", "prod", "int", "lim", "min", "max", "det", "gcd", "lfloor", "rfloor"];
            cmd_name(n).is_some_and(|name| symbol_word(&name).is_some() && !PHRASE_COMMANDS.contains(&name.as_str()))
        }
        NodeOrToken::Node(n) if n.kind() == ItemText => {
            let toks: Vec<_> = n
                .children_with_tokens()
                .filter_map(|e| e.into_token())
                .filter(|t| !matches!(t.kind(), TokenWhiteSpace | TokenLineBreak | TokenComment))
                .collect();
            let boundary = match side {
                Side::Prev => toks.last(),
                Side::Next => toks.first(),
            };
            boundary.is_some_and(|t| t.kind() == TokenWord)
        }
        _ => false,
    }
}

/// Renders a single element in a fresh "nothing spoken yet" context,
/// independent of its actual surroundings — used to compare what two
/// elements *would* sound like on their own, e.g. the two sides of a
/// bare `a / b` division.
fn render_element_alone(element: &Element) -> Result<String> {
    let mut out = String::new();
    speak_element(element, &mut out, false)?;
    Ok(out)
}

/// `phrase` as a `u32` if it's nothing but bare ASCII digits — `None` for
/// anything else (a variable, a negative/glued-operator word, an
/// expression). Used to recognize a fraction's numerator/denominator as a
/// literal integer rather than an arbitrary sub-expression.
fn bare_integer(phrase: &str) -> Option<u32> {
    (!phrase.is_empty() && phrase.chars().all(|c| c.is_ascii_digit())).then(|| phrase.parse().ok()).flatten()
}

/// The named word for a fraction's denominator `n` — "half"/"halves"
/// through "sixteenth"/"sixteenths" (`singular` for a numerator of 1,
/// plural otherwise) — `None` past sixteenths, where there's no common
/// named word and a plain "over" reading is clearer anyway.
fn fraction_denominator_word(n: u32, singular: bool) -> Option<&'static str> {
    Some(match (n, singular) {
        (2, true) => "half",
        (2, false) => "halves",
        (3, true) => "third",
        (3, false) => "thirds",
        (4, true) => "fourth",
        (4, false) => "fourths",
        (5, true) => "fifth",
        (5, false) => "fifths",
        (6, true) => "sixth",
        (6, false) => "sixths",
        (7, true) => "seventh",
        (7, false) => "sevenths",
        (8, true) => "eighth",
        (8, false) => "eighths",
        (9, true) => "ninth",
        (9, false) => "ninths",
        (10, true) => "tenth",
        (10, false) => "tenths",
        (11, true) => "eleventh",
        (11, false) => "elevenths",
        (12, true) => "twelfth",
        (12, false) => "twelfths",
        (13, true) => "thirteenth",
        (13, false) => "thirteenths",
        (14, true) => "fourteenth",
        (14, false) => "fourteenths",
        (15, true) => "fifteenth",
        (15, false) => "fifteenths",
        (16, true) => "sixteenth",
        (16, false) => "sixteenths",
        _ => return None,
    })
}

/// The named-fraction word for `numerator/denominator` (`"1"`/`"2"` ->
/// `Some("half")`), when both sides are bare integer literals and the
/// denominator has a common name (halves through sixteenths) — mirrors
/// the equivalent rule in `odoru`'s text normalizer for plain `N/M` text
/// (see its `dev/normalize.md`, Pass 4b), which can't reach a
/// LaTeX-derived fraction itself: by the time this crate's phrase reaches
/// that normalizer, the `/` is already gone, replaced by the word "over".
/// `None` for a variable numerator/denominator (`n/f_s`), an expression
/// (`n-1`), or a denominator past sixteenths, where the caller should
/// keep the plain "N over M" reading instead.
///
/// The numerator is deliberately left as a bare digit for the caller to
/// push as-is (`"1 half"`, not `"one half"`) — this crate never spells
/// digits into words anywhere else either (e.g. `\frac{\pi}{2}` -> "pi
/// over 2", not "two"); that's the caller's normalizer's job, and it will
/// spell a bare leftover numerator the same way it spells any other bare
/// number in the sentence.
fn named_fraction_word(numerator: &str, denominator: &str) -> Option<&'static str> {
    bare_integer(numerator)?;
    let denominator = bare_integer(denominator)?;
    fraction_denominator_word(denominator, numerator == "1")
}

/// Speaks a fraction given its already-rendered numerator/denominator
/// phrases — shared by `\frac{a}{b}` and the bare `a / b` division path.
/// Picks, in order: "itself" when the two sides render identically (a
/// denominator that would otherwise repeat the numerator verbatim, e.g.
/// `\frac{2^{n-1}}{2^{n-1}}` — repeated identical phrases are both a known
/// TTS/forced-alignment artifact source and slower for a listener to
/// parse than "over itself"), a named fraction word when both sides are
/// small bare integers (see `named_fraction_word`), or a plain "N over M"
/// otherwise.
fn speak_fraction(num_phrase: &str, den_phrase: &str, out: &mut String) {
    push_word(out, num_phrase);
    if den_phrase == num_phrase {
        push_word(out, "over");
        push_word(out, "itself");
    } else if let Some(word) = named_fraction_word(num_phrase, den_phrase) {
        push_word(out, word);
    } else {
        push_word(out, "over");
        push_word(out, den_phrase);
    }
}

/// The word (if any) an `ItemLR`'s opening delimiter contributes, and
/// whether it's conditioned on something already having been spoken.
enum LRPrefix {
    /// Brace/"invisible" delimiters — plain grouping, same as a bare
    /// `{...}` group.
    None,
    /// Function-application delimiters (`(`/`[`): the word only applies
    /// when something was just spoken immediately before, e.g. `x(t)` ->
    /// "x of t" but a leading `(a+b)` is silent grouping.
    IfPreceded(&'static str),
    /// A named operation whose delimiters always speak the same prefix
    /// regardless of what precedes them, e.g. `|x|` -> "the absolute
    /// value of x" whether or not `x` follows other content.
    Always(&'static str),
}

/// Splits an `ItemLR` node (`\left DELIM ... \right DELIM`) into its
/// `LRPrefix` and middle content, as an element list ready for
/// `speak_sequence`.
fn left_right_group(node: &SyntaxNode) -> Result<(LRPrefix, Vec<Element>)> {
    let children: Vec<Element> = node.children_with_tokens().collect();
    let clause_positions: Vec<usize> =
        children.iter().enumerate().filter(|(_, e)| matches!(e, NodeOrToken::Node(n) if n.kind() == ClauseLR)).map(|(i, _)| i).collect();
    let [open_pos, close_pos] = clause_positions.as_slice() else {
        bail!("\\left...\\right group with {} clauses, expected 2", clause_positions.len());
    };

    let NodeOrToken::Node(open_clause) = &children[*open_pos] else { unreachable!() };
    let prefix = open_clause
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find_map(|t| match t.kind() {
            TokenLParen => Some(LRPrefix::IfPreceded("of")),
            TokenLBracket => Some(LRPrefix::IfPreceded("at index")),
            // `|`/`\vert` has no dedicated token kind in mitex_parser — it
            // comes through as a plain `TokenWord` with text `"|"`.
            TokenWord if t.text() == "|" => Some(LRPrefix::Always("the absolute value of")),
            _ => None,
        })
        .unwrap_or(LRPrefix::None);

    let inner = children[open_pos + 1..*close_pos].to_vec();
    Ok((prefix, inner))
}

/// `-`/`+`/`=`/`<`/`>` glued directly into a word (no surrounding spaces)
/// — `mitex` lexes `N-1`, `t+t_0`, `x=y`, `t<0`, and even a leading `-1`
/// as one `TokenWord` each, so there's no separate token to catch these
/// operators generically the way a *spaced* `=` or `<` is (a bare
/// standalone token, handled by `speak_element`'s literal `TokenWord`
/// branch calling this same function). `None` if `word` has none of these
/// characters at all (the common case, left to the caller's literal
/// pass-through). Within math mode specifically (unlike general prose) an
/// embedded `-` essentially never means a hyphenated compound word, so
/// it's safe to always read it as arithmetic: a leading `-` ("-1") is a
/// negative number, an internal one ("N-1") is subtraction; `+`/`=`/`<`/
/// `>` are always spelled as words regardless of position — left as the
/// literal character, some TTS engines (confirmed: misaki/espeak, and the
/// vibe/F5 model) silently produce no phonemes for them at all, dropping
/// them from the audio rather than mispronouncing them.
///
/// `out_has_content` is `speak_sequence`'s own `has_content` for the
/// current sequence being spoken (not just "is `out` non-empty" — `out`
/// often already holds unrelated preceding text, e.g. "the interval from"
/// before a bound's own leading `-\pi`, which must still read "negative")
/// — a `-` at index 0 of *this word* isn't necessarily leading its
/// sequence: `p_1-p_2` glues the second operand's `-` onto `p` (mitex
/// splits it as `-p`, not a separate `-` token) even though `p_1` was
/// already spoken earlier in the same sequence, so word-local position
/// alone would wrongly call it "negative" instead of "minus".
fn speak_operator_word(word: &str, out_has_content: bool) -> Option<String> {
    if !word.contains(['-', '+', '=', '<', '>']) {
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
            '-' if i == 0 && !out_has_content => Some("negative"),
            '-' => Some("minus"),
            '+' => Some("plus"),
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

fn speak_element(element: &Element, out: &mut String, has_content: bool) -> Result<()> {
    match element {
        NodeOrToken::Node(node) => speak_node(node, out, has_content),
        NodeOrToken::Token(tok) => match tok.kind() {
            TokenWhiteSpace | TokenLineBreak | TokenComment => Ok(()),
            TokenWord => {
                // A bare standalone `-` (nothing glued to it at all, e.g.
                // `N - 1`/`a - b - c` — mitex only produces this exact
                // shape when the `-` is a genuine sibling token in *this*
                // same sequence, never as a leftover fragment of some
                // other operand) is safe to resolve from this sequence's
                // own `has_content`: nothing yet means a leading negative
                // sign, anything already spoken here means subtraction.
                //
                // Any other word stays context-free: a self-contained
                // glued word like `-1` right after `=`/`(`/`,` is
                // unambiguous on its own regardless of what was spoken
                // earlier — `x = -1` must stay "negative 1", not "minus
                // 1", even though `has_content` is true by this point.
                // (`speak_attach` special-cases the one remaining
                // ambiguous glued shape, `-p` as an attach's base, itself
                // — see its comment.)
                let context = tok.text() == "-" && has_content;
                if let Some(phrase) = speak_operator_word(tok.text(), context) {
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
            // LaTeX's non-breaking space (`~`) — a spacing/typesetting
            // hint, not something with its own pronunciation.
            TokenTilde => Ok(()),
            // `\begin{...}...\end{...}` environments (align*/split/cases)
            // — see `speak_env`'s doc comment for why both are silent/a
            // pause rather than spoken words.
            TokenAmpersand => Ok(()),
            ItemNewLine => {
                // A semicolon, not a comma — see `speak_env`'s doc comment
                // for why each row gets a stronger break than a comma
                // pause.
                out.push(';');
                Ok(())
            }
            TokenLBrace | TokenRBrace => Ok(()),
            other => bail!("unsupported token in math expression: {other:?} ({:?})", tok.text()),
        },
    }
}

fn speak_node(node: &SyntaxNode, out: &mut String, has_content: bool) -> Result<()> {
    match node.kind() {
        ScopeRoot | ItemFormula => speak_children(node, out),
        ItemCurly => speak_children(node, out),
        ItemText => speak_children(node, out),
        ItemCmd => speak_cmd(node, out),
        ItemAttachComponent => speak_attach(node, out, has_content),
        ItemEnv => speak_env(node, out),
        other => bail!("unsupported math construct: {other:?}"),
    }
}

/// `\begin{env}...\end{env}` — handled uniformly for a whitelist of
/// "linear equation sequence" environments (`align`/`align*`, `split`,
/// `cases`, `aligned`, `eqnarray`/`eqnarray*`, `gather`/`gather*`,
/// `multline`/`multline*`): `env`'s own name (from `ItemBegin`/`ItemEnd`,
/// filtered out here) is purely structural, never spoken; `&` (column
/// alignment, `TokenAmpersand`) carries no meaning of its own either —
/// for `align*`/`split` it just marks where the `=` lines up, and for
/// `cases` the condition after it is already written out in prose by the
/// author (`\text{if } n = 0`), so there's nothing to inject — silent in
/// both. `\\` (row break, `ItemNewLine`) becomes a semicolon rather than a
/// comma — a multi-row sum otherwise reads as a single very long sentence
/// with nothing stronger than comma pauses between rows, which real-world
/// documents have shown produces its own TTS artifacts (observed: the
/// last row getting spoken twice) independent of anything about the math
/// content itself. A semicolon rather than a period deliberately: this
/// crate has no notion of sentence boundaries or capitalization, so it
/// can't correctly start a new sentence after a row break (the next row
/// may not begin with something that should be capitalized) — a
/// semicolon gives a stronger break than a comma without implying one.
///
/// A true grid environment (`matrix`/`pmatrix`/`bmatrix`/`vmatrix`/
/// `Vmatrix`/`smallmatrix`/`array`) is deliberately *not* in that
/// whitelist and stays rejected: flattening rows/columns to comma pauses
/// the same way would lose the 2D structure a matrix's shape actually
/// conveys, rather than just skip a cosmetic alignment mark — this
/// crate's "reject rather than guess" policy applies to it, not the
/// equation-sequence environments above.
fn speak_env(node: &SyntaxNode, out: &mut String) -> Result<()> {
    const EQUATION_SEQUENCE_ENVS: &[&str] = &[
        "align", "align*", "split", "cases", "aligned", "eqnarray", "eqnarray*", "gather",
        "gather*", "multline", "multline*",
    ];
    let env_name = node
        .children()
        .find(|n| n.kind() == ItemBegin)
        .and_then(|begin| begin.children_with_tokens().filter_map(|e| e.into_token()).next())
        .map(|t| t.text().to_string())
        .unwrap_or_default();
    if !EQUATION_SEQUENCE_ENVS.contains(&env_name.as_str()) {
        bail!("unsupported environment: {env_name}");
    }

    let elements: Vec<Element> = node
        .children_with_tokens()
        .filter(|e| !matches!(e, NodeOrToken::Node(n) if n.kind() == ItemBegin || n.kind() == ItemEnd))
        .collect();
    let mut has_content = false;
    speak_sequence(&elements, out, &mut has_content)
}

struct Attach {
    base: SyntaxNode,
    sub: Option<Vec<Element>>,
    sup: Option<Vec<Element>>,
    /// Count of `'` marks attached to `base` (`f'` -> 1, `f''` -> 2, ...).
    /// Unlike `sub`/`sup`, `mitex_parser` gives an apostrophe no scripted
    /// content of its own to capture — it's a bare marker, so this is just
    /// a count rather than an `Option<Vec<Element>>`.
    prime_count: usize,
}

fn speak_attach(node: &SyntaxNode, out: &mut String, has_content: bool) -> Result<()> {
    let attach = parse_attach(node)?;

    // mitex glues a mid-sequence `-` onto the *next* operand's leading
    // token when that operand becomes an attach's base — `p_1-p_2` parses
    // `-p` as one word, the base of the second `_2` attach (and `p_1 -
    // p_2`, spaced, does the same but as a separate `-` token immediately
    // followed by `p`, both inside one `ItemText`) — so that leading `-`
    // isn't really "leading its own expression", it's continuing
    // subtraction from whatever was already spoken (`p_1`). Word-local
    // position alone can't see that (`speak_operator_word` only looks
    // within the one glued word), so it's special-cased directly here:
    // when something was already spoken in the enclosing sequence, strip
    // the base's own leading `-` and speak "minus" before it instead of
    // letting the normal path read it as "negative".
    //
    // Deliberately narrow — this must NOT become "if has_content, treat
    // every leading `-` as minus": a self-contained token like `-1` right
    // after `=`/`(`/`,` (no attach involved at all) is unambiguous on its
    // own and stays "negative" regardless of prior content (`x = -1`
    // stays "x equals negative 1", never "minus 1") — see
    // `speak_element`'s `TokenWord` arm, which always calls
    // `speak_operator_word` context-free for exactly that reason.
    if has_content {
        let flat = flat_base_elements(&attach.base);
        if let Some((NodeOrToken::Token(first_tok), rest)) = flat.split_first() {
            if first_tok.kind() == TokenWord {
                if let Some(after_minus) = first_tok.text().strip_prefix('-') {
                    push_word(out, "minus");
                    if !after_minus.is_empty() {
                        if let Some(phrase) = speak_operator_word(after_minus, false) {
                            push_word(out, &phrase);
                        } else {
                            push_word(out, after_minus);
                        }
                    }
                    let mut rest_has_content = true;
                    speak_sequence(rest, out, &mut rest_has_content)?;
                    return speak_attach_scripts(&attach, out);
                }
            }
        }
    }

    speak_children(&attach.base, out)?;
    speak_attach_scripts(&attach, out)
}

/// The base's real leaf tokens, unwrapping the single `ItemText` wrapper
/// `mitex_parser` adds around a run of words — `speak_children` would
/// reach the same tokens by recursing through `speak_node`'s `ItemText`
/// arm; this does the same unwrap eagerly so `speak_attach` can inspect
/// the very first leaf token without a full recursive walk.
fn flat_base_elements(base: &SyntaxNode) -> Vec<Element> {
    let children: Vec<Element> = base.children_with_tokens().collect();
    if let [NodeOrToken::Node(n)] = children.as_slice() {
        if n.kind() == ItemText {
            return n.children_with_tokens().collect();
        }
    }
    children
}

/// The sub/superscript/prime portion of an `ItemAttachComponent`, without
/// speaking its base — split out from `speak_attach` so a bracket token
/// wrapped in an attach node (`(x-2)^2` gives the closing `)` a `^2`
/// attachment, per `speak_sequence`'s bracket scan) can still speak the
/// script even though the bracket itself is silent/implied.
fn speak_attach_scripts(attach: &Attach, out: &mut String) -> Result<()> {
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
            let mut has_content = false;
            speak_sequence(sup, out, &mut has_content)?;
        }
    }
    if let Some(sub) = &attach.sub {
        if let Some(word) = component_subscript_word(sub) {
            // `x_\perp`/`x_\parallel` name a *component* of `x` (the
            // perpendicular/parallel part of a decomposed vector or
            // signal, common notation in physics/DSP), not a literal
            // subscript index — spoken as "x perpendicular"/"x parallel"
            // directly, not "x sub is perpendicular to" (the relation
            // phrasing `symbol_word` gives `\perp`/`\parallel`
            // elsewhere, for `a \perp b`-style statements).
            push_word(out, word);
        } else {
            push_word(out, "sub");
            let mut has_content = false;
            speak_sequence(sub, out, &mut has_content)?;
        }
    }
    // `f'` -> "f prime", `f''` -> "f double prime", `f'''` -> "f triple
    // prime" (derivative notation) — higher counts are vanishingly rare in
    // practice, so just repeating "prime" is a reasonable fallback rather
    // than a construct worth a full ordinal-naming table.
    match attach.prime_count {
        0 => {}
        1 => push_word(out, "prime"),
        2 => push_word(out, "double prime"),
        3 => push_word(out, "triple prime"),
        n => {
            for _ in 0..n {
                push_word(out, "prime");
            }
        }
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

/// "perpendicular"/"parallel" for a bare `\perp`/`\parallel` subscript
/// (`x_\perp`, `x_\parallel`) — the vector/signal-decomposition component
/// notation, not the relation `a \perp b` (`symbol_word` handles that
/// case, reached via a *sibling* `\perp`, never a subscript). `None` for
/// anything else, so the caller falls back to plain "sub X".
fn component_subscript_word(sub: &[Element]) -> Option<&'static str> {
    let [Element::Node(cmd)] = sub else { return None };
    if cmd.kind() != ItemCmd {
        return None;
    }
    let name_tok =
        cmd.children_with_tokens().filter_map(|e| e.into_token()).find(|t| t.kind() == ClauseCommandName)?;
    match name_tok.text().trim_start_matches('\\') {
        "perp" => Some("perpendicular"),
        "parallel" => Some("parallel"),
        _ => None,
    }
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

/// The bracket kind `e` represents, if any — either a plain bracket token,
/// or (see `speak_sequence`'s bracket scan) a closing bracket that
/// `mitex_parser` wrapped in an `ItemAttachComponent` because it carries a
/// sub/superscript (`(x-2)^2` attaches `^2` to the `)`, not to the whole
/// group). Ignores whatever script the attach carries — the caller decides
/// separately whether to speak it.
fn element_bracket_kind(e: &Element) -> Option<mitex_parser::syntax::SyntaxKind> {
    match e {
        NodeOrToken::Token(t) if matches!(t.kind(), TokenLParen | TokenRParen | TokenLBracket | TokenRBracket) => {
            Some(t.kind())
        }
        NodeOrToken::Node(n) if n.kind() == ItemAttachComponent => {
            let attach = parse_attach(n).ok()?;
            let tokens: Vec<_> = attach.base.children_with_tokens().filter_map(|e| e.into_token()).collect();
            let [only] = tokens.as_slice() else { return None };
            matches!(only.kind(), TokenLParen | TokenRParen | TokenLBracket | TokenRBracket).then(|| only.kind())
        }
        _ => None,
    }
}

/// Scans `elements` (the content just after an already-consumed opening
/// bracket) for the closing bracket of a half-open/half-closed interval —
/// any bracket kind, not just the matching one, found once nested brackets
/// have all closed. Returns the close token's kind and its index within
/// `elements`.
fn find_interval_close(elements: &[Element]) -> Option<(mitex_parser::syntax::SyntaxKind, usize)> {
    let mut depth = 0i32;
    for (idx, e) in elements.iter().enumerate() {
        let Some(kind) = element_bracket_kind(e) else { continue };
        match kind {
            TokenLParen | TokenLBracket => depth += 1,
            TokenRParen | TokenRBracket => {
                if depth == 0 {
                    return Some((kind, idx));
                }
                depth -= 1;
            }
            _ => unreachable!(),
        }
    }
    None
}

/// Expands any top-level `ItemText` node in `elements` into its own child
/// tokens — `mitex_parser` merges a run of words/commas/whitespace into one
/// `ItemText`, so a comma between two interval bounds isn't a flat sibling
/// token until this runs.
fn flatten_text_nodes(elements: &[Element]) -> Vec<Element> {
    let mut out = Vec::with_capacity(elements.len());
    for e in elements {
        if let NodeOrToken::Node(n) = e {
            if n.kind() == ItemText {
                out.extend(n.children_with_tokens());
                continue;
            }
        }
        out.push(e.clone());
    }
    out
}

/// The index of the first top-level comma in `elements` (not nested inside
/// its own bracket pair) — splits an interval's two bounds.
fn top_level_comma(elements: &[Element]) -> Option<usize> {
    let mut depth = 0i32;
    for (idx, e) in elements.iter().enumerate() {
        if let Some(kind) = element_bracket_kind(e) {
            match kind {
                TokenLParen | TokenLBracket => depth += 1,
                TokenRParen | TokenRBracket => depth -= 1,
                _ => unreachable!(),
            }
            continue;
        }
        if depth == 0 {
            if let NodeOrToken::Token(t) = e {
                if t.kind() == TokenComma {
                    return Some(idx);
                }
            }
        }
    }
    None
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
    let mut prime_count = 0usize;
    loop {
        let elements: Vec<Element> = base_node.children_with_tokens().collect();
        let Some(NodeOrToken::Node(arg)) = elements.first() else {
            bail!("attachment with no base");
        };
        let arg = arg.clone();

        let script_start = elements
            .iter()
            .position(|e| matches!(e, NodeOrToken::Token(t) if matches!(t.kind(), TokenUnderscore | TokenCaret | TokenApostrophe)))
            .ok_or_else(|| anyhow::anyhow!("attachment with no _ or ^ token"))?;
        match &elements[script_start] {
            // `'` (derivative notation, `f'`) has no scripted content of
            // its own — `mitex_parser` gives it `has_script: false`, so
            // there's nothing after it at this level to capture, only the
            // mark itself to count.
            NodeOrToken::Token(t) if t.kind() == TokenApostrophe => {
                prime_count += 1;
            }
            NodeOrToken::Token(t) => {
                let is_sub = t.kind() == TokenUnderscore;
                let script_content: Vec<Element> = elements[script_start + 1..].to_vec();
                if is_sub {
                    sub.get_or_insert(script_content);
                } else {
                    sup.get_or_insert(script_content);
                }
            }
            _ => unreachable!(),
        }

        let inner = arg.children().next();
        match inner {
            Some(inner) if inner.kind() == ItemAttachComponent => {
                base_node = inner;
                continue;
            }
            _ => return Ok(Attach { base: arg, sub, sup, prime_count }),
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
            let mut num_phrase = String::new();
            speak_children(num, &mut num_phrase)?;
            let mut den_phrase = String::new();
            speak_children(den, &mut den_phrase)?;
            speak_fraction(&num_phrase, &den_phrase, out);
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
        // `\underbrace{content}_{label}` parses as an ordinary subscript
        // attachment on `\underbrace{content}` (confirmed via the parser
        // directly) — so `\underbrace` itself only needs to speak its
        // content, same as `\text`; the existing sub/sup attach handling
        // in `speak_attach` picks up `_{label}` automatically ("sub
        // label"), with no separate handling needed here.
        "text" | "mathrm" | "mathsf" | "underbrace" => match args.as_slice() {
            [content] => speak_children(content, out),
            other => bail!("\\{name} expects 1 argument, found {}", other.len()),
        },
        "mathbb" => match args.as_slice() {
            // Blackboard-bold almost always names a standard number set in
            // practice — spelling those out ("the real numbers") carries
            // real meaning that just speaking the bare letter wouldn't.
            // Anything else falls back to speaking the content plainly,
            // same as `\mathrm`.
            [content] => match number_set_name(content) {
                Some(name) => {
                    push_word(out, name);
                    Ok(())
                }
                None => speak_children(content, out),
            },
            other => bail!("\\mathbb expects 1 argument, found {}", other.len()),
        },
        // `\{`/`\}` — LaTeX's escaped literal brace, used for set notation
        // (`\{0, 1, 2\}`). Each is its own bare command (no argument),
        // sitting as a sibling next to its content rather than wrapping
        // it, unlike a bare `{...}` group — but the same "silent
        // grouping" treatment applies: the braces themselves aren't
        // spoken, same as `TokenLBrace`/`TokenRBrace` for a plain group.
        "{" | "}" => Ok(()),
        "leftarrow" => {
            // Context this vocabulary was built against is algorithmic
            // assignment (`\hat{X} \leftarrow \text{DFT}(...)`), not a
            // mathematical limit/mapping arrow — "gets" is how that's
            // actually read aloud.
            push_word(out, "gets");
            Ok(())
        }
        "not" => match args.as_slice() {
            // `\not` negates whatever single command follows it
            // (`\not\equiv`, `\not\in`, ...) — recognized relations get a
            // natural negated phrase; anything else falls back to a
            // literal "not" prefix, which reads correctly if awkwardly
            // ("not is equivalent to") rather than failing outright.
            [content] => match negated_relation_word(content) {
                Some(word) => {
                    push_word(out, word);
                    Ok(())
                }
                None => {
                    push_word(out, "not");
                    speak_children(content, out)
                }
            },
            other => bail!("\\not expects 1 argument, found {}", other.len()),
        },
        "overline" => match args.as_slice() {
            // The DSP/signal-processing usage this vocabulary targets
            // (e.g. `\overline{X[N-m]}`) is consistently complex
            // conjugation, not the other common meanings (an average, a
            // repeating decimal) — picking the one actually seen rather
            // than a vaguer "overline of X" that wouldn't convey meaning.
            [content] => {
                push_word(out, "the complex conjugate of");
                speak_children(content, out)
            }
            other => bail!("\\overline expects 1 argument, found {}", other.len()),
        },
        "hat" => match args.as_slice() {
            // Postfix, unlike `\overline`/`\sqrt` — "x hat" is how this is
            // actually said, not "hat of x".
            [content] => {
                speak_children(content, out)?;
                push_word(out, "hat");
                Ok(())
            }
            other => bail!("\\hat expects 1 argument, found {}", other.len()),
        },
        // Pure spacing commands — genuinely no argument, no bearing on how
        // the surrounding math sounds.
        "quad" | ";" => Ok(()),
        // `\phantom{X}` reserves layout space shaped like `X` without
        // rendering it — purely an alignment device, so unlike `\text`
        // etc. its argument must *not* be spoken (that would read content
        // the formula deliberately hides).
        "phantom" | "vphantom" | "hphantom" => match args.as_slice() {
            [_content] => Ok(()),
            other => bail!("\\{name} expects 1 argument, found {}", other.len()),
        },
        // Unlike `\quad`/`\;`, `\displaystyle` is greedy — it consumes
        // everything after it up to the end of its group as one
        // `ClauseArgument` (confirmed: `a \displaystyle b` parses with
        // "b" as `\displaystyle`'s own argument, not a separate sibling).
        // A plain no-op here would silently swallow that content instead
        // of just ignoring the formatting hint.
        "displaystyle" => match args.as_slice() {
            [content] => speak_children(content, out),
            [] => Ok(()),
            other => bail!("\\displaystyle expects 0 or 1 arguments, found {}", other.len()),
        },
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
        _ if is_color_command(name) => {
            // A bare color shorthand (`\red{X}`, `\darkblue{X}`, ...) —
            // not standard LaTeX/xcolor, but a common author convention,
            // and not registered with an argument in `mitex_parser`'s
            // default command spec (unlike `\textcolor{color}{body}`
            // above), so it parses as a bare 0-arg command with the
            // following `{X}` as a separate sibling group, not this
            // command's own `ClauseArgument`. So there's nothing to do
            // here but contribute no words; that sibling `{X}` group
            // already speaks its own content normally via the generic
            // `ItemCurly` handling in `speak_node` — exactly "keep the
            // content, drop the color", since color is a visual cue with
            // no bearing on how the math reads aloud.
            Ok(())
        }
        _ => {
            if let Some(word) = symbol_word(name) {
                // A list separator comma glued directly before the dots
                // family (`1, 0, \dots` -> "1, 0, dot dot dot") leaves a
                // pause immediately followed by three repeats of the same
                // word — a shape TTS backends have been observed to garble.
                // Drop the trailing comma here rather than at the generic
                // `TokenComma` site, so an ordinary `1, 2, \pi` keeps its
                // comma.
                if matches!(name, "dots" | "ldots" | "cdots" | "vdots" | "ddots") && out.ends_with(',') {
                    out.pop();
                }
                push_word(out, word);
                Ok(())
            } else {
                bail!("unsupported command: \\{name}")
            }
        }
    }
}

/// True for a bare color-shorthand command name — `red`, `darkblue`,
/// `lightgray`, etc. Matches xcolor's base palette (the same list
/// `math-render`'s color map supports) with an optional `dark`/`light`
/// prefix, since author documents commonly define exactly those
/// shorthand macros (`\darkblue{...}` etc.) even though they aren't
/// standard LaTeX commands themselves.
fn is_color_command(name: &str) -> bool {
    const BASE_COLORS: &[&str] = &[
        "red", "green", "blue", "cyan", "magenta", "yellow", "black", "white", "gray", "grey",
        "brown", "orange", "pink", "purple", "teal", "olive",
    ];
    let stripped = name.strip_prefix("dark").or_else(|| name.strip_prefix("light")).unwrap_or(name);
    BASE_COLORS.contains(&stripped)
}

/// The spoken name of a standard number set, for `\mathbb{X}` where `X` is
/// exactly one of the conventional letters (`R` -> "the real numbers", ...)
/// — `None` for anything else, so the caller falls back to speaking the
/// bare content.
fn number_set_name(content: &SyntaxNode) -> Option<&'static str> {
    let children: Vec<Element> = content.children_with_tokens().collect();
    let [Element::Node(curly)] = children.as_slice() else { return None };
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
        "R" => Some("the real numbers"),
        "C" => Some("the complex numbers"),
        "N" => Some("the natural numbers"),
        "Z" => Some("the integers"),
        "Q" => Some("the rational numbers"),
        _ => None,
    }
}

/// The natural negated phrase for `\not X`, where `X` (`content`) is
/// exactly one bare, argument-less command — `\not\equiv` -> "is not
/// equivalent to", etc. `None` for anything else (a non-command argument,
/// or a command `\not` doesn't have a specific phrase for), so the caller
/// falls back to a literal "not" prefix.
fn negated_relation_word(content: &SyntaxNode) -> Option<&'static str> {
    let children: Vec<Element> = content.children_with_tokens().collect();
    let [Element::Node(cmd)] = children.as_slice() else { return None };
    if cmd.kind() != ItemCmd {
        return None;
    }
    let name_tok = cmd
        .children_with_tokens()
        .filter_map(|e| e.into_token())
        .find(|t| t.kind() == ClauseCommandName)?;
    match name_tok.text().trim_start_matches('\\') {
        "equiv" => Some("is not equivalent to"),
        "in" => Some("is not an element of"),
        "leq" | "le" => Some("is not less than or equal to"),
        "geq" | "ge" => Some("is not greater than or equal to"),
        "propto" => Some("is not proportional to"),
        "perp" => Some("is not perpendicular to"),
        "parallel" => Some("is not parallel to"),
        _ => None,
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
        "sim" => "on the order of",
        "ll" => "is much less than",
        "gg" => "is much greater than",
        "circ" => "circle",
        "pm" => "plus or minus",
        "Delta" => "delta",
        "ell" => "ell",
        "sharp" => "sharp",
        "angle" => "angle",
        "equiv" => "is equivalent to",
        "perp" => "is perpendicular to",
        "parallel" => "is parallel to",
        "mod" => "mod",
        "in" => "is an element of",
        "notin" => "is not an element of",
        "mid" => "such that",
        "rightarrow" | "to" => "goes to",
        "Rightarrow" => "implies",
        "Leftarrow" => "is implied by",
        "Leftrightarrow" => "if and only if",
        "lfloor" => "the floor of",
        "rfloor" => "",
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
    fn fraction_repeated_denominator_speaks_anaphorically() {
        assert_eq!(
            speak(r"\frac{2^{n-1}}{2^{n-1}}").unwrap(),
            "2 to the n minus 1 over itself"
        );
    }

    #[test]
    fn frac_small_named_fractions() {
        assert_eq!(speak(r"\frac{1}{2}").unwrap(), "1 half");
        assert_eq!(speak(r"\frac{3}{2}").unwrap(), "3 halves");
        assert_eq!(speak(r"\frac{2}{3}").unwrap(), "2 thirds");
        assert_eq!(speak(r"\frac{1}{16}").unwrap(), "1 sixteenth");
        assert_eq!(speak(r"\frac{5}{37}").unwrap(), "5 over 37");
    }

    #[test]
    fn frac_named_fraction_requires_bare_integers() {
        // A variable or expression on either side keeps the plain "over"
        // reading — only a literal integer numerator/denominator qualify.
        assert_eq!(speak(r"\frac{n}{2}").unwrap(), "n over 2");
        assert_eq!(speak(r"\frac{1}{f_s}").unwrap(), "1 over f sub s");
    }

    #[test]
    fn slash_division_small_named_fraction() {
        assert_eq!(speak("1 / 2").unwrap(), "1 half");
        assert_eq!(speak("2 / 3").unwrap(), "2 thirds");
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
        assert_eq!(speak("(a+b)").unwrap(), "a plus b");
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
        assert_eq!(speak(r"\red{x} + \blue{y}").unwrap(), "x plus y");
        assert_eq!(speak(r"\textcolor{red}{x} + y").unwrap(), "x plus y");
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
        assert_eq!(speak(r"0, 1, 2, \dots, N-1").unwrap(), "0, 1, 2 dot dot dot, N minus 1");
    }

    #[test]
    fn standalone_spaced_minus_between_plain_operands() {
        assert_eq!(speak("N - 1").unwrap(), "N minus 1");
        assert_eq!(speak("a - b - c").unwrap(), "a minus b minus c");
    }

    #[test]
    fn subtraction_between_subscripted_operands() {
        // The reported bug: mitex glues the `-` onto the next operand's
        // base when it's subscripted (`p_1-p_2` -> base `-p` for the
        // second attach), or splits it into its own token immediately
        // followed by the base (`p_1 - p_2`, spaced) -- both must read as
        // subtraction, not a leading negative sign on the second operand.
        assert_eq!(speak("p_1 - p_2").unwrap(), "p sub 1 minus p sub 2");
        assert_eq!(speak("p_1-p_2").unwrap(), "p sub 1 minus p sub 2");
        assert_eq!(speak("z_{m} - z_{k}").unwrap(), "z sub m minus z sub k");
        assert_eq!(speak("p_1 - p_2 + p_3").unwrap(), "p sub 1 minus p sub 2 plus p sub 3");
    }

    #[test]
    fn leading_negative_sign_on_subscripted_operand_unaffected() {
        assert_eq!(speak("-p_1").unwrap(), "negative p sub 1");
        assert_eq!(speak("-p_1 - p_2").unwrap(), "negative p sub 1 minus p sub 2");
    }

    #[test]
    fn leading_negative_after_relation_stays_negative_not_minus() {
        // Must NOT regress: `has_content` being true (from "x ="` already
        // spoken) must not make a self-contained `-1` read as "minus 1".
        assert_eq!(speak("x = -1").unwrap(), "x equals negative 1");
    }

    #[test]
    fn perp_and_parallel_component_subscripts_speak_as_bare_words() {
        // `x_\perp`/`x_\parallel` name a vector/signal decomposition
        // component ("the perpendicular part of x"), not a literal
        // subscript index or the `a \perp b` relation -- must not say
        // "sub" or "is ... to".
        assert_eq!(speak(r"x_\perp(t)").unwrap(), "x perpendicular of t");
        assert_eq!(speak(r"x_\perp").unwrap(), "x perpendicular");
        assert_eq!(speak(r"x_\parallel").unwrap(), "x parallel");
        // A real numeric/variable subscript is unaffected.
        assert_eq!(speak(r"x_1").unwrap(), "x sub 1");
    }

    #[test]
    fn leading_negative_inside_fresh_grouping_contexts() {
        // Parens/brackets/sqrt/frac/sup content all start a fresh
        // "has anything been spoken" context, independent of what
        // preceded the group itself.
        assert_eq!(speak(r"x(-1)").unwrap(), "x of negative 1");
        assert_eq!(speak(r"x[-1]").unwrap(), "x at index negative 1");
        assert_eq!(speak(r"\sqrt{-1}").unwrap(), "the square root of negative 1");
        assert_eq!(speak(r"\frac{1}{2} - p_1").unwrap(), "1 half minus p sub 1");
        assert_eq!(speak(r"e^{-1}").unwrap(), "e to the negative 1");
    }

    #[test]
    fn plus_sign_spoken_spaced_or_glued() {
        assert_eq!(speak("t + t_0").unwrap(), "t plus t sub 0");
        assert_eq!(speak("t+t_0").unwrap(), "t plus t sub 0");
    }

    // The real reported case: repeated periodicity equation with `+`
    // between the shifted-time terms.
    #[test]
    fn periodicity_equation_speaks_plus() {
        assert_eq!(
            speak(r"x(t) = x(t + t_0) = x(t + 2\cdot t_0) = x(t + 3\cdot t_0) = \dots").unwrap(),
            "x of t equals x of t plus t sub 0 equals x of t plus 2, t sub 0 \
             equals x of t plus 3, t sub 0 equals dot dot dot"
        );
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
    fn slash_division_repeated_operand_speaks_anaphorically() {
        assert_eq!(speak("2^{n-1} / 2^{n-1}").unwrap(), "2 to the n minus 1 over itself");
    }

    #[test]
    fn cdot_between_atomic_factors_is_silent() {
        assert_eq!(speak(r"2 \cdot \pi \cdot f_1 \cdot t").unwrap(), "2, pi, f sub 1, t");
    }

    #[test]
    fn cdot_before_named_function_is_silent() {
        assert_eq!(speak(r"A_1 \cdot \cos(x)").unwrap(), "A sub 1, cosine of x");
    }

    #[test]
    fn cdot_boundary_word_in_larger_text_run_is_still_detected() {
        // `t + \phi_1` parses as one `ItemText` node (`t`, `+`, `\phi_1`),
        // not separate siblings — only `t`, the token actually touching
        // `\cdot`, should matter for the silence decision.
        assert_eq!(speak(r"f_1 \cdot t + \phi_1").unwrap(), "f sub 1, t plus phi sub 1");
    }

    #[test]
    fn cdot_between_compound_expressions_keeps_times() {
        assert_eq!(speak(r"(a+b) \cdot (c+d)").unwrap(), "a plus b times c plus d");
    }

    #[test]
    fn cdot_next_to_phrase_command_keeps_times() {
        assert_eq!(speak(r"\sqrt{2} \cdot 3").unwrap(), "the square root of 2 times 3");
    }

    #[test]
    fn times_command_always_spoken() {
        assert_eq!(speak(r"3 \times 4").unwrap(), "3 times 4");
    }

    #[test]
    fn cdot_before_grouping_parens_is_not_function_application() {
        assert_eq!(speak(r"V \cdot \left(\frac{a}{b}\right)").unwrap(), "V times a over b");
    }

    #[test]
    fn equals_before_grouping_parens_is_not_function_application() {
        assert_eq!(speak(r"x = (a+b)").unwrap(), "x equals a plus b");
    }

    #[test]
    fn absolute_value_bars() {
        assert_eq!(speak(r"\left|x\right|").unwrap(), "the absolute value of x");
    }

    #[test]
    fn absolute_value_bars_of_function_call() {
        assert_eq!(
            speak(r"\left|v\left(-2^{n-1}\right)\right|").unwrap(),
            "the absolute value of v of negative 2 to the n minus 1"
        );
    }

    #[test]
    fn left_right_delimiters() {
        // The motivating real-world case: units in brackets.
        assert_eq!(speak(r"\left[\frac{\text{W}}{\text{m}^2}\right]").unwrap(), "W over m squared");
        // Same function-application/grouping rules as bare `(`/`[` apply.
        assert_eq!(speak(r"x\left(t\right)").unwrap(), "x of t");
        assert_eq!(speak(r"\left(a+b\right)").unwrap(), "a plus b");
        assert_eq!(speak(r"\left[a, b\right]").unwrap(), "a, b");
    }

    #[test]
    fn proportional_to() {
        assert_eq!(speak(r"I \propto p^2").unwrap(), "I is proportional to p squared");
    }

    #[test]
    fn generalized_color_commands() {
        assert_eq!(speak(r"\purple{x}").unwrap(), "x");
        assert_eq!(speak(r"\darkblue{y}").unwrap(), "y");
        assert_eq!(speak(r"\magenta{z}").unwrap(), "z");
        assert_eq!(speak(r"\green{a} \cyan{b}").unwrap(), "a b");
        // Not a recognized color name — still rejected, not guessed at.
        assert!(speak(r"\notacolor{x}").is_err());
    }

    #[test]
    fn overline_and_hat() {
        assert_eq!(speak(r"\overline{X[N-m]}").unwrap(), "the complex conjugate of X at index N minus m");
        assert_eq!(speak(r"\hat{x}").unwrap(), "x hat");
    }

    #[test]
    fn spacing_commands_are_silent() {
        assert_eq!(speak(r"a \quad b").unwrap(), "a b");
        assert_eq!(speak(r"a \; b").unwrap(), "a b");
        assert_eq!(speak(r"a \displaystyle b").unwrap(), "a b");
    }

    #[test]
    fn phantom_content_is_never_spoken() {
        assert_eq!(speak(r"a \phantom{x} b").unwrap(), "a b");
        assert_eq!(speak(r"a \vphantom{x} b").unwrap(), "a b");
    }

    #[test]
    fn additional_symbols() {
        assert_eq!(speak(r"d \in N").unwrap(), "d is an element of N");
        assert_eq!(speak(r"m \notin S").unwrap(), "m is not an element of S");
        assert_eq!(speak(r"\theta \rightarrow \theta + \phi").unwrap(), "theta goes to theta plus phi");
        assert_eq!(speak(r"a \equiv b \mod n").unwrap(), "a is equivalent to b mod n");
        assert_eq!(speak(r"\Delta").unwrap(), "delta");
        assert_eq!(speak(r"\ell").unwrap(), "ell");
        assert_eq!(speak(r"\angle").unwrap(), "angle");
        assert_eq!(speak(r"a \perp b").unwrap(), "a is perpendicular to b");
        assert_eq!(speak(r"a \parallel b").unwrap(), "a is parallel to b");
        assert_eq!(speak(r"\lfloor x \rfloor").unwrap(), "the floor of x");
    }

    #[test]
    fn prime_notation() {
        assert_eq!(speak("f'").unwrap(), "f prime");
        assert_eq!(speak("N' < N").unwrap(), "N prime less than N");
        assert_eq!(speak("n''").unwrap(), "n double prime");
        assert_eq!(speak("n'''").unwrap(), "n triple prime");
        // Sub/superscript still resolve normally alongside a prime.
        assert_eq!(speak("f'_s").unwrap(), "f sub s prime");
    }

    #[test]
    fn escaped_set_braces_are_silent_grouping() {
        assert_eq!(speak(r"\{0, 1, 2\}").unwrap(), "0, 1, 2");
        assert_eq!(speak(r"m \notin \{0, N\}").unwrap(), "m is not an element of 0, N");
    }

    #[test]
    fn mathbb_number_sets() {
        assert_eq!(speak(r"z \in \mathbb{C}").unwrap(), "z is an element of the complex numbers");
        assert_eq!(speak(r"\theta \in \mathbb{R}").unwrap(), "theta is an element of the real numbers");
        assert_eq!(speak(r"d \in \mathbb{N}").unwrap(), "d is an element of the natural numbers");
        // Not one of the conventional letters — falls back to plain content.
        assert_eq!(speak(r"\mathbb{X}").unwrap(), "X");
    }

    #[test]
    fn leftarrow_is_assignment() {
        assert_eq!(speak(r"X \leftarrow \text{DFT}(x)").unwrap(), "X gets DFT of x");
    }

    #[test]
    fn not_negates_known_relations() {
        assert_eq!(speak(r"\theta \not\equiv 0").unwrap(), "theta is not equivalent to 0");
        // Falls back to a literal "not" prefix for a relation without a
        // specific negated phrase, rather than failing outright.
        assert_eq!(speak(r"a \not\propto b").unwrap(), "a is not proportional to b");
    }

    #[test]
    fn underbrace_speaks_content() {
        assert_eq!(speak(r"\underbrace{0, 0}").unwrap(), "0, 0");
    }

    #[test]
    fn align_environment_row_break_is_a_semicolon() {
        assert_eq!(speak(r"\begin{align*} a &= 1\\ b &= 2\\ c &= 3 \end{align*}").unwrap(), "a equals 1; b equals 2; c equals 3");
    }

    #[test]
    fn trailing_plus_before_row_break_moves_after_the_semicolon() {
        // The `+` continuing a sum onto the next row belongs to that next
        // row's clause, not the one ending — "a; plus, b" reads correctly,
        // "a plus; b" doesn't (the connective word stays glued to the
        // wrong side of the pause).
        assert_eq!(speak(r"\begin{align*} a \;+\\ &b \end{align*}").unwrap(), "a; plus, b");
    }

    #[test]
    fn split_environment() {
        assert_eq!(speak(r"\begin{split} x &= 1\\ &= 2 \end{split}").unwrap(), "x equals 1; equals 2");
    }

    #[test]
    fn align_environment() {
        assert_eq!(speak(r"\begin{align*} a &= 1\\ b &= 2 \end{align*}").unwrap(), "a equals 1; b equals 2");
    }

    #[test]
    fn cases_environment() {
        // The author already writes "if"/"otherwise" as prose (`\text{if
        // }`), so the environment itself contributes only the row-break
        // semicolon between the two cases — no connective word is
        // injected.
        assert_eq!(
            speak(r"\begin{cases} 1 & \text{if } n = 0\\ 0 & \text{otherwise}. \end{cases}").unwrap(),
            "1 if n equals 0; 0 otherwise ."
        );
    }

    // The real motivating case: a multi-line sum with `=&` alignment
    // (equals sign before the column break) and a trailing "+" continuing
    // onto the next row.
    #[test]
    fn align_environment_real_world_sum() {
        assert_eq!(
            speak(
                r"\begin{align*} x(t) =& A_1 \cdot \cos(2\pi \cdot f_1 \cdot t + \phi_1) \;+\\ &A_2\cdot \cos(2\pi \cdot f_2\cdot t + \phi_2) \;+\\ &A_3\cdot \cos(2\pi \cdot f_3\cdot t + \phi_3) + \cdots \end{align*}"
            )
            .unwrap(),
            "x of t equals A sub 1, cosine of 2 pi, f sub 1, t plus phi sub 1; \
             plus, A sub 2, cosine of 2 pi, f sub 2, t plus phi sub 2; \
             plus, A sub 3, cosine of 2 pi, f sub 3, t plus phi sub 3 plus dot dot dot"
        );
    }

    #[test]
    fn bare_equals_sign_spoken_as_word() {
        assert_eq!(speak("y = g(x)").unwrap(), "y equals g of x");
    }

    #[test]
    fn ellipsis_commands() {
        assert_eq!(speak(r"0, 1, 2, \dots").unwrap(), "0, 1, 2 dot dot dot");
        assert_eq!(speak(r"\cdots").unwrap(), "dot dot dot");
    }

    // The reported case: a comma directly before the dots family leaves a
    // pause immediately followed by three repeats of the same word, which
    // TTS backends have been observed to garble.
    #[test]
    fn ellipsis_drops_preceding_comma() {
        assert_eq!(
            speak(r"x[n] = 1, 0, -1, 0, 1, 0, -1, 0, \dots").unwrap(),
            "x at index n equals 1, 0, negative 1, 0, 1, 0, negative 1, 0 dot dot dot"
        );
    }

    // Unbraced `n^th` is standard LaTeX for `n^t` followed by a plain
    // trailing "h" — a superscript with no braces only ever applies to the
    // single next character. Not this crate's call to special-case.
    #[test]
    fn unbraced_ordinal_only_takes_one_character() {
        assert_eq!(speak("n^th").unwrap(), "n to the t h");
    }

    #[test]
    fn on_the_order_of_symbol() {
        assert_eq!(speak(r"\sim N^2").unwrap(), "on the order of N squared");
    }

    #[test]
    fn much_less_than_symbol() {
        assert_eq!(speak(r"N \ll N^2").unwrap(), "N is much less than N squared");
    }

    #[test]
    fn much_greater_than_symbol() {
        assert_eq!(speak(r"N \gg 1").unwrap(), "N is much greater than 1");
    }

    #[test]
    fn circle_symbol() {
        assert_eq!(speak(r"\circ").unwrap(), "circle");
    }

    #[test]
    fn degree_symbol_still_takes_priority_over_circle_word() {
        assert_eq!(speak(r"360^\circ").unwrap(), "360 degrees");
    }

    #[test]
    fn closing_bracket_with_exponent() {
        assert_eq!(speak(r"(x-2)^2").unwrap(), "x minus 2 squared");
    }

    #[test]
    fn closing_index_bracket_with_exponent() {
        assert_eq!(speak(r"x[n]^2").unwrap(), "x at index n squared");
    }

    #[test]
    fn half_open_interval_bracket_paren() {
        assert_eq!(speak(r"[a, b)").unwrap(), "the interval from a inclusive, to b exclusive");
    }

    #[test]
    fn half_open_interval_paren_bracket() {
        assert_eq!(speak(r"(a, b]").unwrap(), "the interval from a exclusive, to b inclusive");
    }

    #[test]
    fn half_open_interval_with_signed_bounds() {
        assert_eq!(
            speak(r"[-\pi, +\pi)").unwrap(),
            "the interval from negative pi inclusive, to plus pi exclusive"
        );
    }

    #[test]
    fn interval_notation_inside_larger_expression() {
        assert_eq!(
            speak(r"x \in [a, b)").unwrap(),
            "x is an element of the interval from a inclusive, to b exclusive"
        );
    }

    #[test]
    fn plain_closed_bracket_pair_is_still_silent_grouping() {
        assert_eq!(speak(r"[a, b]").unwrap(), "a, b");
    }

    #[test]
    fn braces_dont_trigger_of_on_the_paren_they_precede() {
        assert_eq!(speak(r"\frac{1}{(x-2)}").unwrap(), "1 over x minus 2");
    }
}
