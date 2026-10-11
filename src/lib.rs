//! b4: read Banking4 vault files.
//!
//! `b4 decrypt` turns a `.sub` vault into a DuckDB database; the other commands
//! query that database.
//!
//! The crate is layered: [`vault`] reads the encrypted container and knows
//! nothing about records, [`wire`] decodes the primitives records are made of,
//! [`model`] turns record bytes into plain structs and knows nothing about the
//! database. The command-line interface sits on top: `cli` defines the
//! commands, `commands` runs them, `output` renders their results and
//! `password` obtains the vault password.
//!
//! The format is described in `docs/format/` of the repository. The library
//! API is not stable yet.

pub mod model;
pub mod vault;
pub mod wire;

mod cli;
mod commands;
mod db;
mod error;
mod load;
mod output;
mod password;

pub use cli::Cli;
pub use error::Error;

/// Run the command described by `cli`.
///
/// On failure the caller prints the error with [`Error::report`] and ends the
/// process with [`Error::exit_code`].
pub fn run(cli: Cli) -> Result<(), Error> {
    cli.validate().map_err(Error::usage)?;
    commands::run(&cli)
}
