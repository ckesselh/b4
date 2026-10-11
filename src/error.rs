//! The error a command ends with, and how it reaches the user.
//!
//! Exit statuses: 0 success, 1 a runtime error (the message is on stderr),
//! 2 a usage error. clap reports most usage errors itself, before a command
//! runs; [`Error::usage`] is for the few that need more than one argument to
//! detect.

use std::fmt::Display;
use std::io::{self, IsTerminal, Write};

use clap::builder::styling::{AnsiColor, Style};

use crate::vault::VaultError;

/// Exit status of a command that failed at run time.
pub const EXIT_FAILURE: u8 = 1;

/// Exit status of a command that was called wrongly. The same as clap's.
pub const EXIT_USAGE: u8 = 2;

/// The style of the `error:` prefix on a terminal: the only colour b4 uses.
pub const ERROR_STYLE: Style = AnsiColor::Red.on_default().bold();

/// A source chain longer than this is cut off. No real chain comes close; the
/// limit only keeps a faulty `source()` from looping.
const MAX_CHAIN: usize = 16;

/// A failed command: a message for stderr and the exit status of the process.
///
/// Lower layers have their own error types. They become an `Error` through
/// [`Error::from_error`], which keeps the whole source chain in the message:
///
/// ```ignore
/// impl From<vault::Error> for Error {
///     fn from(error: vault::Error) -> Self {
///         Error::from_error(&error)
///     }
/// }
/// ```
///
/// With such a conversion in place, `?` works and `Context::context` adds
/// what the command was doing.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    message: String,
    exit_code: u8,
}

impl Error {
    /// A runtime error: exit status 1.
    pub fn new(message: impl Into<String>) -> Error {
        Error {
            message: message.into(),
            exit_code: EXIT_FAILURE,
        }
    }

    /// A usage error that clap could not detect: exit status 2.
    pub fn usage(message: impl Into<String>) -> Error {
        Error {
            message: message.into(),
            exit_code: EXIT_USAGE,
        }
    }

    /// End the command with `exit_code` and without a message. For results
    /// that are a status and not a failure, such as `status --check` finding
    /// the database stale, and for a reader that closed the pipe.
    pub fn exit(exit_code: u8) -> Error {
        Error {
            message: String::new(),
            exit_code,
        }
    }

    /// A runtime error with the message of `error` and of every error behind
    /// it, joined as `a: b: c`.
    ///
    /// A source whose text the message before it already ends with is left
    /// out, so error types that repeat their source in their own message do
    /// not print it twice.
    pub fn from_error(error: &(dyn std::error::Error + 'static)) -> Error {
        let mut message = error.to_string();
        let mut source = error.source();
        for _ in 0..MAX_CHAIN {
            let Some(cause) = source else { break };
            let text = cause.to_string();
            if !text.is_empty() && !message.ends_with(&text) {
                message.push_str(": ");
                message.push_str(&text);
            }
            source = cause.source();
        }
        Error::new(message)
    }

    /// The same error with `context` in front: `context: message`.
    pub fn context(mut self, context: impl Display) -> Error {
        if !self.is_silent() {
            self.message = format!("{context}: {}", self.message);
        }
        self
    }

    /// The same error with another exit status.
    pub fn with_exit_code(mut self, exit_code: u8) -> Error {
        self.exit_code = exit_code;
        self
    }

    /// The exit status the process ends with.
    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }

    /// Whether there is nothing to print.
    pub fn is_silent(&self) -> bool {
        self.message.is_empty()
    }

    /// Print `error: message` to standard error, the prefix in red when
    /// `stderr_colour` allows it. A silent error prints nothing.
    pub fn report(&self) {
        // Nothing useful can be done if stderr is gone.
        let _ = self.write_report(&mut io::stderr().lock(), stderr_colour());
    }

    fn write_report(&self, out: &mut impl Write, colour: bool) -> io::Result<()> {
        if self.is_silent() {
            return Ok(());
        }
        // A message can quote text from a vault or a database, which may hold
        // escape sequences: never pass those on to a terminal.
        let message = crate::output::message_text(&self.message);
        if colour {
            let style = ERROR_STYLE;
            writeln!(out, "{style}error:{style:#} {message}")
        } else {
            writeln!(out, "error: {message}")
        }
    }
}

impl From<io::Error> for Error {
    fn from(error: io::Error) -> Error {
        Error::from_error(&error)
    }
}

/// What to tell the user when a vault's structure does not add up: Banking4
/// writes data, directory and table pages one after the other, so a vault it
/// has open can be inconsistent for a moment without being damaged (01 §11).
const VAULT_IN_USE_HINT: &str =
    "the vault may be open in Banking4 right now and in the middle of a write, try again";

impl From<VaultError> for Error {
    fn from(error: VaultError) -> Error {
        let hint = matches!(
            error,
            VaultError::CorruptAllocationTable { .. }
                | VaultError::CorruptChain { .. }
                | VaultError::CorruptDirectory { .. }
                | VaultError::CrossLinked { .. }
        );
        let error = Error::from_error(&error);
        if hint {
            Error::new(format!("{}: {VAULT_IN_USE_HINT}", error.message))
        } else {
            error
        }
    }
}

/// What to tell the user when an entry b4 reads leads into the storage of
/// one it does not read. A vault that Banking4 is writing can in principle
/// show this for a moment like any other cross-link (01 §11), so that is
/// named too, but second: Banking4 is not known to write anything that looks
/// like it.
const FENCED_HINT: &str = "the vault is damaged or was manipulated, unless Banking4 is writing \
                           it at this moment: b4 reads nothing from an entry that shares storage \
                           with one it does not read";

impl From<crate::load::LoadError> for Error {
    /// The container's error behind the name of the entry that could not be
    /// read, followed by the hint that fits: that the vault may be in use,
    /// if the error can be a passing state, or that it is damaged or
    /// manipulated, for an entry that runs into storage b4 keeps away from.
    fn from(error: crate::load::LoadError) -> Error {
        let context = error.to_string();
        match error {
            crate::load::LoadError::Entry { source, .. } => Error::from(source).context(context),
            crate::load::LoadError::Fenced { source, .. } => Error::new(format!(
                "{context}: {}: {FENCED_HINT}",
                Error::from_error(&source).message
            )),
        }
    }
}

impl From<duckdb::Error> for Error {
    /// The database's own message. Not [`Error::from_error`]: the source of
    /// a DuckDB error is its bare status code, which adds nothing.
    fn from(error: duckdb::Error) -> Error {
        Error::new(crate::db::read::message_of(&error))
    }
}

/// Adds what the command was doing to an error on its way up.
pub trait Context<T> {
    /// Turn the error into an [`Error`] that reads `context: message`.
    fn context(self, context: impl Display) -> Result<T, Error>;
}

impl<T, E: Into<Error>> Context<T> for Result<T, E> {
    fn context(self, context: impl Display) -> Result<T, Error> {
        self.map_err(|error| error.into().context(context))
    }
}

/// Whether the `error:` prefix on standard error is coloured.
///
/// Only when standard error is a terminal, and not when the user opted out:
/// `NO_COLOR` set to anything but the empty string (the convention of
/// no-color.org), or a terminal that says it cannot do it (`TERM=dumb`).
pub fn stderr_colour() -> bool {
    io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none_or(|value| value.is_empty())
        && std::env::var_os("TERM").is_none_or(|term| term != "dumb")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, thiserror::Error)]
    #[error("cannot read the header")]
    struct Outer(#[source] Middle);

    #[derive(Debug, thiserror::Error)]
    #[error("cluster chain is broken")]
    struct Middle(#[source] io::Error);

    /// An error type that repeats its source in its own message.
    #[derive(Debug, thiserror::Error)]
    #[error("cannot open the database: {0}")]
    struct Repeating(#[source] io::Error);

    fn report(error: &Error, colour: bool) -> String {
        let mut out = Vec::new();
        error.write_report(&mut out, colour).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn from_error_joins_the_source_chain() {
        let inner = io::Error::other("unexpected end of file");
        let error = Error::from_error(&Outer(Middle(inner)));
        assert_eq!(
            error.to_string(),
            "cannot read the header: cluster chain is broken: unexpected end of file"
        );
        assert_eq!(error.exit_code(), EXIT_FAILURE);
    }

    #[test]
    fn from_error_does_not_repeat_a_source_already_in_the_message() {
        let error = Error::from_error(&Repeating(io::Error::other("permission denied")));
        assert_eq!(
            error.to_string(),
            "cannot open the database: permission denied"
        );
    }

    #[test]
    fn vault_errors_keep_their_message_and_structure_errors_get_a_hint() {
        use crate::vault::{ChainFault, TableFault};

        let plain = Error::from(VaultError::NotAVault);
        assert_eq!(
            plain.to_string(),
            "not a Banking4 vault: the file does not start with a container header"
        );
        assert_eq!(plain.exit_code(), EXIT_FAILURE);
        assert_eq!(
            Error::from(VaultError::WrongPassword).to_string(),
            "wrong password"
        );

        // The source of an I/O error is in the message once.
        let io = VaultError::Io {
            path: "x.sub".into(),
            source: io::Error::other("permission denied"),
        };
        assert_eq!(
            Error::from(io).to_string(),
            "cannot read vault file x.sub: permission denied"
        );

        // 01 §11: these four can be the passing state of a vault in use.
        let hinted = [
            VaultError::CorruptAllocationTable {
                cluster: 4,
                fault: TableFault::BadStatus,
            },
            VaultError::CorruptChain {
                cluster: 9,
                fault: ChainFault::EndsEarly,
            },
            VaultError::CorruptDirectory {
                cluster: 9,
                fault: ChainFault::Loop,
            },
            VaultError::CrossLinked {
                cluster: 9,
                first: "the root directory".to_owned(),
                second: "entry \"Stmt\"".to_owned(),
            },
        ];
        for error in hinted {
            let own = error.to_string();
            let text = Error::from(error).to_string();
            assert_eq!(text, format!("{own}: {VAULT_IN_USE_HINT}"));
        }
        assert_eq!(
            Error::from(VaultError::CorruptChain {
                cluster: 9,
                fault: ChainFault::Loop,
            })
            .to_string(),
            "corrupt cluster chain: the chain loops (cluster 9): the vault may be open in \
             Banking4 right now and in the middle of a write, try again"
        );
        // An entry that leads into storage b4 does not read says what that
        // means first.
        let fenced = crate::load::LoadError::Fenced {
            entry: "AcctGroups".to_owned(),
            source: VaultError::CrossLinked {
                cluster: 528,
                first: "entry \"Contacts\"".to_owned(),
                second: "entry \"AcctGroups\"".to_owned(),
            },
        };
        assert_eq!(
            Error::from(fenced).to_string(),
            "cannot read the vault entry AcctGroups: corrupt container: entry \"Contacts\" and \
             entry \"AcctGroups\" share cluster 528: the vault is damaged or was manipulated, \
             unless Banking4 is writing it at this moment: b4 reads nothing from an entry that \
             shares storage with one it does not read"
        );
        let entry = crate::load::LoadError::Entry {
            entry: "Payees".to_owned(),
            source: VaultError::CorruptChain {
                cluster: 9,
                fault: ChainFault::Loop,
            },
        };
        assert_eq!(
            Error::from(entry).to_string(),
            format!(
                "cannot read the vault entry Payees: corrupt cluster chain: the chain loops \
                 (cluster 9): {VAULT_IN_USE_HINT}"
            )
        );
        // A truncated file is not a passing state.
        let text = Error::from(VaultError::Truncated { cluster: 128 }).to_string();
        assert!(!text.contains("Banking4 right now"), "{text}");
    }

    #[test]
    fn context_goes_in_front_and_keeps_the_exit_code() {
        let error = Error::usage("--from is after --to").context("bookings list");
        assert_eq!(error.to_string(), "bookings list: --from is after --to");
        assert_eq!(error.exit_code(), EXIT_USAGE);

        let result: Result<(), io::Error> = Err(io::Error::other("no such file"));
        let error = result.context("cannot open vault.sub").unwrap_err();
        assert_eq!(error.to_string(), "cannot open vault.sub: no such file");
        assert_eq!(error.exit_code(), EXIT_FAILURE);
    }

    #[test]
    fn report_has_the_prefix_and_colour_only_on_request() {
        let error = Error::new("database not found");
        assert_eq!(report(&error, false), "error: database not found\n");

        let coloured = report(&error, true);
        assert_eq!(
            coloured,
            "\x1b[1m\x1b[31merror:\x1b[0m database not found\n"
        );
        // Only the prefix is coloured.
        assert!(coloured.ends_with("\x1b[0m database not found\n"));
    }

    #[test]
    fn a_silent_error_prints_nothing() {
        let error = Error::exit(1);
        assert!(error.is_silent());
        assert_eq!(error.exit_code(), 1);
        assert_eq!(report(&error, true), "");
        // Context has nothing to attach to.
        assert!(error.context("status").is_silent());
    }

    #[test]
    fn exit_code_can_be_changed() {
        assert_eq!(Error::new("stale").with_exit_code(3).exit_code(), 3);
    }
}
