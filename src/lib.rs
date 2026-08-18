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

fn speak_children(node: &SyntaxNode, out: &mut String) -> Result<()> {
    for child in node.children_with_tokens() {
        speak_element(&child, out)?;
    }
    Ok(())
}

fn speak_element(element: &NodeOrToken<SyntaxNode, mitex_parser::syntax::SyntaxToken>, out: &mut String) -> Result<()> {
    match element {
        NodeOrToken::Node(node) => speak_node(node, out),
        NodeOrToken::Token(tok) => match tok.kind() {
            TokenWhiteSpace | TokenLineBreak | TokenComment => Ok(()),
            TokenWord => {
                push_word(out, tok.text());
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
        ItemText => {
            for tok in node
                .children_with_tokens()
                .filter_map(|e| e.into_token())
            {
                if tok.kind() == TokenWord {
                    push_word(out, tok.text());
                }
            }
            Ok(())
        }
        ItemCmd => speak_cmd(node, out),
        ItemAttachComponent => speak_attach(node, out),
        other => bail!("unsupported math construct: {other:?}"),
    }
}

/// The base and each attached sub/superscript of an `ItemAttachComponent`.
/// `base` is the node this attachment applies to; `sub`/`sup` are the
/// scripted content (everything after the `_`/`^` token), phrased as
/// "sub X" / "to the X" by the caller.
type Element = NodeOrToken<SyntaxNode, mitex_parser::syntax::SyntaxToken>;

struct Attach {
    base: SyntaxNode,
    sub: Option<Vec<Element>>,
    sup: Option<Vec<Element>>,
}

fn speak_attach(node: &SyntaxNode, out: &mut String) -> Result<()> {
    let attach = parse_attach(node)?;
    speak_children(&attach.base, out)?;
    if let Some(sup) = &attach.sup {
        if let Some(power) = simple_power_word(sup) {
            push_word(out, power);
        } else {
            push_word(out, "to the");
            for el in sup {
                speak_element(el, out)?;
            }
        }
    }
    if let Some(sub) = &attach.sub {
        push_word(out, "sub");
        for el in sub {
            speak_element(el, out)?;
        }
    }
    Ok(())
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
        "times" => "times",
        "cdot" => "times",
        "pm" => "plus or minus",
        "sum" => "the sum of",
        "prod" => "the product of",
        "int" => "the integral of",
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
}
