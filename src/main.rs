use std::process::ExitCode;

fn main() -> ExitCode {
    let tex = match std::env::args().nth(1) {
        Some(tex) => tex,
        None => {
            eprintln!("usage: math-to-speech '<latex>'");
            return ExitCode::FAILURE;
        }
    };

    let (tex, stripped) = math_to_speech::strip_math_delimiters(&tex);
    if let Some((open, close)) = stripped {
        eprintln!("stripped outer delimiter '{open}...{close}'");
    }

    match math_to_speech::speak(tex) {
        Ok(phrase) => {
            println!("{phrase}");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
