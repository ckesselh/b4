//! `b4 completions`: print a shell completion script.

use clap::CommandFactory;

use crate::cli::{Cli, CompletionsArgs, Globals, Shell};
use crate::output::print_bytes;
use crate::Error;

/// The name the script completes.
const BIN_NAME: &str = "b4";

/// Print the completion script for the shell of `args` to standard output.
///
/// The script is generated from the same clap command that parses the
/// arguments, so it knows every command and option of this very binary.
pub fn run(args: &CompletionsArgs, _globals: &Globals) -> Result<(), Error> {
    let shell = match args.shell {
        Shell::Bash => clap_complete::Shell::Bash,
        Shell::Zsh => clap_complete::Shell::Zsh,
        Shell::Fish => clap_complete::Shell::Fish,
    };
    // Into memory first: the generator panics if it cannot write, and a
    // closed pipe on stdout must end quietly.
    let mut script = Vec::new();
    clap_complete::generate(shell, &mut Cli::command(), BIN_NAME, &mut script);
    print_bytes(&script)
}
