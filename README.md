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
letters and common symbols, `\text{...}`). It's the same LaTeX subset
[`math-render`](../math-render) (LaTeX → SVG, in this same parent
directory) targets, so a document's formulas can be rendered and spoken
from the same source without either path silently diverging on what
"supported LaTeX" means.

## Status

Early — covers fractions, `\sqrt`, sub/superscripts (including `x^2` /
`x^3` → "squared"/"cubed"), sums/products/integrals as prefix phrases,
`\text{}`/`\mathrm{}`, and a fixed list of Greek letters and common
symbols/relations. Not yet supported:
* matrices, 
* aligned/multi-line equations, and
* cases/piecewise definitions.

Unsupported cases currently return an `Err` rather than a mis-spoken result.
