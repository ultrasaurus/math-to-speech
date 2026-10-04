# math-to-speech

LaTeX math → spoken English text, entirely in Rust.

```rust
let phrase = math_to_speech::speak(r"\frac{\pi}{2}")?;
assert_eq!(phrase, "pi over 2");
```

General-purpose document TTS (Microsoft Edge, Adobe Acrobat) reads a
compiled LaTeX formula's rendered glyphs literally, ignoring what the
formula means. This crate instead walks the formula's parsed structure and
emits the phrase a person would say aloud — `x^2` becomes "x squared",
`\sqrt{2}` becomes "the square root of 2".

## Usage

```rust
use math_to_speech::speak;

assert_eq!(speak(r"\frac{\pi}{2}")?, "pi over 2");
assert_eq!(speak("x^2")?, "x squared");
assert_eq!(speak("x_i")?, "x sub i");
assert_eq!(speak(r"\sqrt{2}")?, "the square root of 2");
assert_eq!(speak(r"\alpha \leq \beta")?, "alpha less than or equal to beta");
assert_eq!(speak("x(t)")?, "x of t");
assert_eq!(speak("x[n]")?, "x at index n");
assert_eq!(speak(r"\sin(x)")?, "sine of x");
```

* `tex` is the LaTeX math source without surrounding delimiters (`$...$`,
  `\(...\)`, `\[...\]`).
* Unrecognized commands or unsupported constructs return an `Err` rather
  than guessing — callers get a clean signal to fall back (e.g. speak a
  placeholder, or the raw LaTeX) instead of receiving a spoken phrase that
  quietly mangles the formula's meaning.

## How it works

[`mitex-parser`](https://github.com/mitex-rs/mitex) parses the LaTeX math
source into an AST; this crate walks that tree and emits a phrase per
construct (fractions, exponents/subscripts, roots, sums/integrals, Greek
letters, named functions, and common symbols, `\text{...}`). It's the same
LaTeX subset [`math-render`](../math-render) (LaTeX → SVG, in this same
parent directory) targets, so a document's formulas can be rendered and
spoken from the same source without either path silently diverging on what
"supported LaTeX" means.

`(`/`[` aren't grouped into their own node by `mitex-parser` — they're
plain sibling tokens next to whatever's inside them — so this crate tracks
bracket depth itself, then phrases a bracket as function application only
when something was just spoken immediately before it:
* `x(t)` → "x of t",
* `x[n]` → "x at index n" (kept distinct from parens so continuous- and
discrete-time signal notation don't collapse to the same phrase),
* `\sin(x)` → "sine of x".

A bracket with nothing before it, or only an operator/relation like `=`
or `+`, is plain grouping instead:
* `[a, b]` → "a, b",
* `x = (a+b)` → "x equals a plus b"

A number or another bracket group before a bracket means multiplication,
not function application: `2(a+b)` → "2 times the quantity a plus b".

Since the brackets are silent, a group holding more than one term joined
by an operator is spoken as "the quantity ..." wherever its extent would
otherwise be ambiguous — divided, dividing, raised to a power, or
multiplied — with a comma closing it when more follows:
* `a/(b \times c)` → "a over the quantity b times c",
* `(a+b)^2/c` → "the quantity a plus b, squared, over c",
* `a/(b+c) + d` → "a over the quantity b plus c, plus d",
* `a/(b)` → "a over b", `a/(2\pi)` → "a over 2 pi" (one term).

`\frac` numerators/denominators follow the same rule, so `\frac{a+b}{c}`
and `(a+b)/c` read identically.

Parens/brackets themselves stay silent. Only parenthesized function application
is handled; a bare `\sin x` (no parens) currently speaks as "sine x",
not "sine of x".

## Status

Early — covers:
* fractions, `\sqrt`,
* sub/superscripts (including `x^2` / `x^3` → "squared"/"cubed"),
* sums/products/integrals as prefix phrases,
* `\text{}`/`\mathrm{}`,
* named functions (`\sin`, `\cos`, `\tan`, `\log`, `\lim`, etc. —
  parenthesized calls only),
* function-application and discrete-index bracket notation (`x(t)`,
  `x[n]`), and
* a fixed list of Greek letters and common symbols/relations.

Not yet supported:
* matrices,
* aligned/multi-line equations, and
* cases/piecewise definitions.

Unsupported cases currently return an `Err` rather than a mis-spoken result.
