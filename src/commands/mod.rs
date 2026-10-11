//! One module per command.
//!
//! [`run`] dispatches the parsed command line to `<command>::run(args,
//! globals)`. A resource with several verbs (`accounts`, `bookings`, ...) is
//! one module that dispatches once more to a function per verb.
//!
//! Every command follows the same conventions:
//!
//! * Results go to standard output through [`crate::output`]: build a
//!   [`Table`](crate::output::Table) or a [`Record`](crate::output::Record)
//!   and print it with `print_table` / `print_record`, which honour
//!   `--output`, `--no-header` and `--quiet` and cope with a closed pipe.
//!   Nothing else is written to standard output, and never with `println!`,
//!   which panics when the reader has gone.
//! * Messages go to standard error: `output::note` for what `--quiet`
//!   silences, `output::warning` for warnings.
//! * Failures are returned as [`Error`]; `main` prints them and sets the exit
//!   status. A lower-layer error gets a `From` conversion built on
//!   [`Error::from_error`].
//! * Only `decrypt`, `info` and `status` ever open a vault, and only `decrypt`
//!   needs its password. Every other command opens the database of
//!   `globals.db` with [`Database::open`](crate::db::read::Database::open):
//!   read-only and sandboxed (the file system disabled, so no other file, no
//!   `ATTACH`, no extension and no network; the configuration locked; at most
//!   1 GiB of memory and nothing spilled to disk). A file that is missing,
//!   that is no database written by b4 or that has another schema version is
//!   rejected there, with the same message for every command and the advice
//!   to run `b4 decrypt`.
//!
//! # Account references
//!
//! Where a command takes an `ACCOUNT` (`accounts show`, `--account`), the text
//! is resolved against the `accounts` table in this order, and the first rule
//! that matches at least one account decides:
//!
//! 1. the numeric id;
//! 2. the IBAN, compared without blanks and without regard to case;
//! 3. the account number;
//! 4. a case-insensitive substring of the account name. When several names
//!    contain the text and exactly one of them is the text, that complete
//!    name wins over the partial matches.
//!
//! A reference that still matches several accounts is an error that lists
//! the candidates with their ids; one that matches none is an error too.
//! Hidden accounts can be referenced like any other. All commands share one
//! implementation of this rule, `account_ref`.

mod account_ref;
mod accounts;
mod booking_filter;
mod bookings;
mod categories;
mod completions;
mod decrypt;
mod export;
mod info;
mod payees;
mod portfolio;
mod record;
mod schema;
mod sql;
mod standing_orders;
mod status;
#[cfg(test)]
mod testdb;

use crate::cli::{Cli, Command};
use crate::Error;

/// Dispatch to the command selected on the command line.
pub fn run(cli: &Cli) -> Result<(), Error> {
    let globals = &cli.globals;
    match &cli.command {
        Command::Decrypt(args) => decrypt::run(args, globals),
        Command::Info(args) => info::run(args, globals),
        Command::Status(args) => status::run(args, globals),
        Command::Accounts(command) => accounts::run(command, globals),
        Command::Bookings(command) => bookings::run(command, globals),
        Command::Payees(command) => payees::run(command, globals),
        Command::Categories(command) => categories::run(command, globals),
        Command::StandingOrders(command) => standing_orders::run(command, globals),
        Command::Portfolio(command) => portfolio::run(command, globals),
        Command::Sql(args) => sql::run(args, globals),
        Command::Schema(args) => schema::run(args, globals),
        Command::Export(args) => export::run(args, globals),
        Command::Completions(args) => completions::run(args, globals),
    }
}
