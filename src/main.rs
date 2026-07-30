use std::process::ExitCode;

fn main() -> ExitCode {
    match mux::run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("mux: {error:#}");
            ExitCode::FAILURE
        }
    }
}
