use std::process::ExitCode;

use b4::{run, Cli};

fn main() -> ExitCode {
    match run(Cli::from_command_line()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error.report();
            ExitCode::from(error.exit_code())
        }
    }
}
