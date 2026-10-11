//! Where the vault password comes from.
//!
//! There are four sources, tried in this order:
//!
//! 1. `--password-stdin`: the first line of standard input;
//! 2. the environment variable `B4_PASSWORD`;
//! 3. a prompt without echo on the controlling terminal;
//! 4. when there is no terminal: a program that asks for the password, as
//!    ssh and git do it (see below).
//!
//! The password is never a command-line argument, because arguments are
//! visible to every process on the machine and end up in the shell history,
//! and it is never stored.
//!
//! # The program that asks
//!
//! A command that a desktop launcher, a scheduler or an agent starts has no
//! terminal to ask on. For that case b4 runs a program that puts the question
//! to the user, typically in a dialog: the one named by the environment
//! variable `B4_ASKPASS`, or else, in a graphical session (`WAYLAND_DISPLAY`
//! or `DISPLAY` is set), the one named by `SSH_ASKPASS`. The program gets the
//! question as its only argument, prints the password as one line on its
//! standard output and ends with status 0; any other status means that the
//! user cancelled. This is the convention of `ssh-askpass`, so the programs
//! written for ssh, git and sudo fit.
//!
//! b4 asks on the terminal whenever it has one, also when standard input is
//! redirected, and a prompt that the user ends with Ctrl-D or Ctrl-C is an
//! answer: no program is asked after it. `SSH_ASKPASS_REQUIRE=never`, which
//! tells ssh not to use its program, keeps b4 from borrowing it too.
//!
//! The password goes from that program to b4 through a pipe, so it does not
//! appear in the arguments, the environment or the output of whatever started
//! b4. That is not a protection against the one who starts b4: the
//! environment b4 is started with decides which program is run, and b4 runs
//! it as it is.
//!
//! # The environment variable
//!
//! The variable is the weakest of the four sources, and the help of
//! `b4 decrypt` says so. The environment a process is started with can be
//! read by every process of the same user for as long as the process lives
//! (`/proc/<pid>/environ` on Linux, `ps e` elsewhere), it is inherited by
//! everything the process starts, and it tends to stay set in the shell that
//! exported it. Standard input and the prompt have none of these problems.
//!
//! What b4 can do about it, it does: [`obtain`] removes the variable from
//! the environment of the process as soon as it has read it, whichever
//! source the password then comes from. From then on nothing in the process
//! can read it back, and no process b4 might start inherits it. It cannot
//! take back what the kernel recorded when the process started: on Linux
//! `/proc/<pid>/environ` keeps showing the initial environment until the
//! process ends.
//!
//! A vault can have an empty password. An empty password is accepted from
//! every source, but only when it was really given: an empty line on standard
//! input, a variable that is set to nothing, Enter at the prompt, an empty
//! line from the program that asks. Standard input that ends without a single
//! byte is not a password, it is a pipeline whose left side failed, and the
//! same goes for a program that prints nothing.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io::{self, BufRead, Read};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use zeroize::{Zeroize, Zeroizing};

use crate::error::Error;

/// The environment variable that can hold the password.
pub const ENV_VAR: &str = "B4_PASSWORD";

/// The environment variable that names the program b4 runs to ask for the
/// password when it has no terminal.
pub const ASKPASS_ENV_VAR: &str = "B4_ASKPASS";

/// The variable ssh uses for the same purpose. b4 falls back to it in a
/// graphical session.
const SSH_ASKPASS_ENV_VAR: &str = "SSH_ASKPASS";

/// The variable with which ssh is told when to use its program. The value
/// `never` keeps b4 from borrowing the program as well.
const SSH_ASKPASS_REQUIRE_ENV_VAR: &str = "SSH_ASKPASS_REQUIRE";

/// The variables that tell a graphical session.
const DISPLAY_ENV_VARS: [&str; 2] = ["WAYLAND_DISPLAY", "DISPLAY"];

/// The longest password accepted on standard input or from the program that
/// asks for it, in bytes. It bounds what is read when the input is not a
/// password at all.
const MAX_STDIN_BYTES: usize = 4096;

/// The most characters of a vault's file name that are shown in the question
/// a program asks.
const MAX_NAME_CHARS: usize = 80;

/// What the user is asked on the terminal.
const PROMPT: &str = "Vault password: ";

/// A vault password. The memory is overwritten when the value is dropped, and
/// `Debug` does not show it.
pub struct Password(Zeroizing<String>);

impl Password {
    fn new(password: String) -> Password {
        Password(Zeroizing::new(password))
    }

    /// The password itself. Do not copy it into a `String` that outlives the
    /// key derivation.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Password {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Password(<redacted>)")
    }
}

/// Why no password could be obtained.
#[derive(Debug, thiserror::Error)]
pub enum PasswordError {
    /// `--password-stdin` was given and standard input was empty.
    #[error(
        "no password on standard input (--password-stdin reads its first line; \
         send an empty line for an empty password)"
    )]
    StdinEmpty,

    /// The first line of standard input is longer than a password can be.
    #[error("the first line of standard input is longer than {MAX_STDIN_BYTES} bytes")]
    StdinTooLong,

    /// Standard input could not be read.
    #[error("cannot read the password from standard input")]
    StdinRead(#[source] io::Error),

    /// The bytes from the named source are not text.
    #[error("the password from {0} is not valid UTF-8")]
    NotUtf8(&'static str),

    /// None of the sources is available.
    #[error(
        "no password available: pipe it to --password-stdin, set the environment \
         variable {ENV_VAR}, run b4 in a terminal to be asked for it, or name a \
         program that asks for it in {ASKPASS_ENV_VAR}"
    )]
    Unavailable(#[source] io::Error),

    /// The program that asks for the password could not be started.
    #[error("cannot run {program}, the program {variable} names to ask for the password")]
    AskpassStart {
        /// The program as the variable names it.
        program: String,
        /// The variable that names it.
        variable: &'static str,
        /// Why it could not be started.
        #[source]
        source: io::Error,
    },

    /// The output of the program that asks for the password could not be read.
    #[error("cannot read the password from {program}")]
    AskpassRead {
        /// The program as the variable names it.
        program: String,
        /// Why reading failed.
        #[source]
        source: io::Error,
    },

    /// The program ended with a status other than 0: the user cancelled, or
    /// the program could not ask.
    #[error(
        "no password: {program}, the program {variable} names to ask for it, \
         was cancelled or failed ({status})"
    )]
    AskpassFailed {
        /// The program as the variable names it.
        program: String,
        /// The variable that names it.
        variable: &'static str,
        /// How the program ended.
        status: ExitStatus,
    },

    /// The program printed more than a password can be.
    #[error("{program} printed more than {MAX_STDIN_BYTES} bytes where a password was expected")]
    AskpassTooLong {
        /// The program as the variable names it.
        program: String,
    },

    /// The program ended with status 0 and printed nothing at all.
    #[error(
        "no password: {program}, the program {variable} names to ask for it, printed \
         nothing (a password is one line; an empty line is an empty password)"
    )]
    AskpassEmpty {
        /// The program as the variable names it.
        program: String,
        /// The variable that names it.
        variable: &'static str,
    },

    /// The program printed bytes that are not text.
    #[error("the password that {program} printed is not valid UTF-8")]
    AskpassNotUtf8 {
        /// The program as the variable names it.
        program: String,
    },

    /// The user ended the prompt on the terminal without an answer.
    #[error("no password: the prompt on the terminal was cancelled")]
    PromptCancelled,

    /// There is a terminal, and asking on it failed.
    #[error("cannot read the password from the terminal")]
    PromptFailed(#[source] io::Error),
}

/// How asking on the terminal ended.
enum Prompted {
    /// What the user typed.
    Password(String),
    /// There is no terminal to ask on.
    NoTerminal(io::Error),
    /// There is a terminal, and no password came from it.
    Failed(io::Error),
}

impl From<PasswordError> for Error {
    fn from(error: PasswordError) -> Error {
        match &error {
            // The reason the prompt failed ("no such device") explains
            // nothing to the user; the message already says what to do.
            PasswordError::Unavailable(_) => Error::new(error.to_string()),
            _ => Error::from_error(&error),
        }
    }
}

/// Obtain the password of `vault`. `from_stdin` is the `--password-stdin`
/// flag. The vault's file name is only used in the question a program asks.
///
/// The environment variable [`ENV_VAR`] is removed from the environment of
/// the process, whether or not the password comes from it.
///
/// Call this before the process has a second thread, which for b4 means
/// before the database is opened (DuckDB starts worker threads); see
/// [`take_env`].
pub fn obtain(from_stdin: bool, vault: &Path) -> Result<Password, Error> {
    let password = resolve(
        from_stdin,
        &mut io::stdin().lock(),
        take_env(ENV_VAR),
        prompt_on_terminal,
        || {
            let (program, variable) = askpass_program(|name| std::env::var_os(name))?;
            Some(run_askpass(&program, variable, &question(vault)))
        },
    )?;
    Ok(password)
}

/// Ask on the controlling terminal, without echo.
///
/// Whether there is a terminal is found out first and on its own, because
/// the answer decides what a failure means. Without a terminal a program may
/// ask instead. With one, a prompt that fails was answered: by Ctrl-D, by
/// Ctrl-C, or by a terminal that cannot be read, and none of these is a
/// reason to put the question a second time somewhere else.
fn prompt_on_terminal() -> Prompted {
    if let Err(none) = controlling_terminal() {
        return Prompted::NoTerminal(none);
    }
    match rpassword::prompt_password(PROMPT) {
        Ok(password) => Prompted::Password(password),
        Err(error) => Prompted::Failed(error),
    }
}

/// Whether the process has a terminal to ask on: the prompt opens the same
/// device.
#[cfg(unix)]
fn controlling_terminal() -> io::Result<()> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")
        .map(drop)
}

/// Elsewhere the prompt itself is the only test there is.
#[cfg(not(unix))]
fn controlling_terminal() -> io::Result<()> {
    Ok(())
}

/// The program to ask for the password with and the variable that names it:
/// [`ASKPASS_ENV_VAR`], or in a graphical session `SSH_ASKPASS`, unless
/// `SSH_ASKPASS_REQUIRE` is `never`. `None` when neither applies. A variable
/// that is set to nothing counts as not set.
fn askpass_program(lookup: impl Fn(&str) -> Option<OsString>) -> Option<(OsString, &'static str)> {
    let set = |name: &str| lookup(name).filter(|value| !value.is_empty());
    if let Some(program) = set(ASKPASS_ENV_VAR) {
        return Some((program, ASKPASS_ENV_VAR));
    }
    // Whoever told ssh never to use its program does not want b4 to.
    if set(SSH_ASKPASS_REQUIRE_ENV_VAR).is_some_and(|value| value == "never") {
        return None;
    }
    // Without a display the program of ssh could not show its dialog, and
    // its failure would hide the plain "no password available".
    if DISPLAY_ENV_VARS.iter().any(|name| set(name).is_some()) {
        return set(SSH_ASKPASS_ENV_VAR).map(|program| (program, SSH_ASKPASS_ENV_VAR));
    }
    None
}

/// What the program asks: it names b4 and the vault's file, so that a dialog
/// that appears on its own says where it comes from. The name is cut to
/// [`MAX_NAME_CHARS`] characters, and the characters a display acts on
/// instead of showing them are replaced, because a dialog shows the text as
/// it gets it: control characters, the overrides of the text direction, and
/// the line and paragraph separators, which would let a name add lines of its
/// own to the question.
///
/// The question begins and ends the same way whatever the file is called.
/// Programs of this kind guess from the wording what they are asked for:
/// ksshaskpass, for one, shows what is typed when the question ends in
/// `Verification code: `, and offers to remember the answer when it reads
/// like a question of ssh or git. A file name must not be able to steer
/// that, so it stands in quotes between a fixed beginning and a fixed end.
fn question(vault: &Path) -> String {
    let name = vault
        .file_name()
        .unwrap_or(vault.as_os_str())
        .to_string_lossy();
    let name: String = name
        .chars()
        .take(MAX_NAME_CHARS)
        .map(|c| {
            if crate::output::acts_on_terminal(c) || matches!(c, '\u{2028}' | '\u{2029}') {
                '?'
            } else {
                c
            }
        })
        .collect();
    format!("b4: password for the Banking4 vault \"{name}\": ")
}

/// Run `program` with `question` as its only argument and take the first
/// line of its standard output as the password. The program inherits
/// standard error, so that what it has to say reaches the user, and gets no
/// standard input.
fn run_askpass(
    program: &OsStr,
    variable: &'static str,
    question: &str,
) -> Result<Password, PasswordError> {
    let name = || program.to_string_lossy().into_owned();

    let mut child = Command::new(program)
        .arg(question)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|source| PasswordError::AskpassStart {
            program: name(),
            variable,
            source,
        })?;

    // Reserved in full and read into directly, so that the bytes exist in
    // this one buffer only, which is wiped. One byte more than a password
    // with its line ending can have shows that there is too much.
    let mut line = Zeroizing::new(Vec::with_capacity(MAX_STDIN_BYTES + 64));
    let limit = (MAX_STDIN_BYTES + 3) as u64;
    let read = match child.stdout.take() {
        Some(stdout) => stdout.take(limit).read_to_end(&mut line),
        None => Err(io::Error::other("the program has no standard output")),
    };
    // The pipe is closed now. A program that still writes ends on that, so
    // waiting for it cannot block on a full pipe.
    let status = child.wait();

    read.map_err(|source| PasswordError::AskpassRead {
        program: name(),
        source,
    })?;
    let status = status.map_err(|source| PasswordError::AskpassRead {
        program: name(),
        source,
    })?;
    if !status.success() {
        return Err(PasswordError::AskpassFailed {
            program: name(),
            variable,
            status,
        });
    }

    // Nothing at all is not an answer: a dialog that could not open, or a
    // script around one that lost its status. An empty line is.
    if line.is_empty() {
        return Err(PasswordError::AskpassEmpty {
            program: name(),
            variable,
        });
    }

    // The first line without its line ending, as on standard input.
    if let Some(end) = line.iter().position(|&byte| byte == b'\n') {
        line.truncate(end);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    if line.len() > MAX_STDIN_BYTES {
        return Err(PasswordError::AskpassTooLong { program: name() });
    }
    match String::from_utf8(std::mem::take(&mut *line)) {
        Ok(password) => Ok(Password::new(password)),
        Err(error) => {
            error.into_bytes().zeroize();
            Err(PasswordError::AskpassNotUtf8 { program: name() })
        }
    }
}

/// The value of the environment variable `name`, which is removed from the
/// environment of the process: afterwards nothing can read it there, and a
/// process started from this one does not inherit it.
///
/// The caller wipes the returned value; [`resolve`] does.
///
/// Changing the environment is only sound while no other thread reads or
/// writes it, because the C library offers no locking for it. In edition
/// 2021 `std::env::remove_var` is still a safe function (the crate forbids
/// `unsafe`), which leaves that condition to the caller. It holds here:
/// `b4 decrypt` asks for the password first, when the process is still the
/// one thread `main` started on. The Rust runtime starts no threads of its
/// own, the prompt (`rpassword`) starts none, and DuckDB, which does, is only
/// opened once the vault has been decrypted with the password.
fn take_env(name: &str) -> Option<OsString> {
    let value = std::env::var_os(name)?;
    std::env::remove_var(name);
    Some(value)
}

/// The password from the first available source. The sources are parameters
/// so that tests need neither a terminal nor the process environment.
fn resolve(
    from_stdin: bool,
    stdin: &mut dyn BufRead,
    env: Option<OsString>,
    prompt: impl FnOnce() -> Prompted,
    askpass: impl FnOnce() -> Option<Result<Password, PasswordError>>,
) -> Result<Password, PasswordError> {
    if from_stdin {
        // Not used, but still a copy of a password: wipe it.
        if let Some(unused) = env {
            unused.into_encoded_bytes().zeroize();
        }
        return read_first_line(stdin);
    }
    if let Some(value) = env {
        return match value.into_string() {
            Ok(password) => Ok(Password::new(password)),
            Err(raw) => {
                // Not text, so not a password b4 can use; wipe it all the same.
                raw.into_encoded_bytes().zeroize();
                Err(PasswordError::NotUtf8(
                    "the environment variable B4_PASSWORD",
                ))
            }
        };
    }
    // The prompt uses the controlling terminal, so it works while stdin or
    // stdout are redirected. Only without a terminal is a program asked to
    // put the question, as ssh does it.
    match prompt() {
        Prompted::Password(password) => Ok(Password::new(password)),
        Prompted::NoTerminal(none) => match askpass() {
            Some(asked) => asked,
            None => Err(PasswordError::Unavailable(none)),
        },
        // Ctrl-D on an empty line, or Ctrl-C where it does not end b4.
        Prompted::Failed(error)
            if matches!(
                error.kind(),
                io::ErrorKind::UnexpectedEof | io::ErrorKind::Interrupted
            ) =>
        {
            Err(PasswordError::PromptCancelled)
        }
        Prompted::Failed(error) => Err(PasswordError::PromptFailed(error)),
    }
}

/// The first line of `stdin` without its line ending: one `\n` or `\r\n` is
/// removed, nothing else. Whatever follows the first line is left unread.
fn read_first_line(stdin: &mut dyn BufRead) -> Result<Password, PasswordError> {
    // Reserved in full, so that the buffer never moves and leaves a copy of
    // the password behind. (The buffer inside the standard library's stdin
    // cannot be wiped from safe code.)
    let mut line = Zeroizing::new(Vec::with_capacity(MAX_STDIN_BYTES + 2));
    // One byte more than allowed, plus the line feed, shows an over-long line.
    let limit = (MAX_STDIN_BYTES + 2) as u64;
    let read = (&mut *stdin)
        .take(limit)
        .read_until(b'\n', &mut line)
        .map_err(PasswordError::StdinRead)?;
    if read == 0 {
        return Err(PasswordError::StdinEmpty);
    }

    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    }
    if line.len() > MAX_STDIN_BYTES {
        return Err(PasswordError::StdinTooLong);
    }

    // `from_utf8` takes over the allocation, so the password is not copied.
    match String::from_utf8(std::mem::take(&mut *line)) {
        Ok(password) => Ok(Password::new(password)),
        Err(error) => {
            error.into_bytes().zeroize();
            Err(PasswordError::NotUtf8("standard input"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_prompt() -> Prompted {
        panic!("the prompt must not be reached")
    }

    /// What opening the terminal reports when there is none: ENXIO, "No such
    /// device or address".
    fn no_terminal() -> Prompted {
        Prompted::NoTerminal(io::Error::from_raw_os_error(6))
    }

    /// No program is named that could ask.
    fn no_program() -> Option<Result<Password, PasswordError>> {
        None
    }

    fn not_asked() -> Option<Result<Password, PasswordError>> {
        panic!("the program that asks must not be reached")
    }

    fn from_stdin(input: &[u8]) -> Result<Password, PasswordError> {
        resolve(true, &mut &*input, None, no_prompt, not_asked)
    }

    #[test]
    fn stdin_takes_the_first_line_only() {
        assert_eq!(from_stdin(b"secret\n").unwrap().expose(), "secret");
        assert_eq!(
            from_stdin(b"secret\nsecond line\n").unwrap().expose(),
            "secret"
        );
        assert_eq!(from_stdin(b"secret").unwrap().expose(), "secret");
    }

    #[test]
    fn stdin_strips_exactly_one_line_ending() {
        assert_eq!(from_stdin(b"secret\r\n").unwrap().expose(), "secret");
        // Only one: a second carriage return belongs to the password, and so
        // do blanks and a carriage return without a line feed.
        assert_eq!(from_stdin(b"secret\r\r\n").unwrap().expose(), "secret\r");
        assert_eq!(from_stdin(b" secret \n").unwrap().expose(), " secret ");
        assert_eq!(from_stdin(b"secret\r").unwrap().expose(), "secret\r");
    }

    #[test]
    fn stdin_leaves_the_rest_unread() {
        let mut input: &[u8] = b"secret\nselect 1\n";
        let password = resolve(true, &mut input, None, no_prompt, not_asked).unwrap();
        assert_eq!(password.expose(), "secret");
        assert_eq!(input, b"select 1\n");
    }

    #[test]
    fn an_empty_line_is_an_explicit_empty_password() {
        assert_eq!(from_stdin(b"\n").unwrap().expose(), "");
        assert_eq!(from_stdin(b"\r\n").unwrap().expose(), "");
        assert_eq!(from_stdin(b"\nmore").unwrap().expose(), "");
    }

    #[test]
    fn empty_stdin_is_not_a_password() {
        assert!(matches!(from_stdin(b""), Err(PasswordError::StdinEmpty)));
    }

    #[test]
    fn stdin_accepts_non_ascii_and_rejects_invalid_utf8() {
        assert_eq!(
            from_stdin("pässwörd €\n".as_bytes()).unwrap().expose(),
            "pässwörd €"
        );
        assert!(matches!(
            from_stdin(b"\xff\xfe\n"),
            Err(PasswordError::NotUtf8(_))
        ));
    }

    #[test]
    fn stdin_is_bounded() {
        let longest = vec![b'x'; MAX_STDIN_BYTES];
        assert_eq!(
            from_stdin(&longest).unwrap().expose().len(),
            MAX_STDIN_BYTES
        );

        let mut with_ending = longest.clone();
        with_ending.extend_from_slice(b"\r\n");
        assert_eq!(
            from_stdin(&with_ending).unwrap().expose().len(),
            MAX_STDIN_BYTES
        );

        let too_long = vec![b'x'; MAX_STDIN_BYTES + 1];
        assert!(matches!(
            from_stdin(&too_long),
            Err(PasswordError::StdinTooLong)
        ));

        // Endless input without a line feed stops after the limit.
        let mut endless = io::repeat(b'x');
        let mut reader = io::BufReader::new(&mut endless);
        let result = resolve(true, &mut reader, None, no_prompt, not_asked);
        assert!(matches!(result, Err(PasswordError::StdinTooLong)));
    }

    #[test]
    fn stdin_wins_over_the_environment() {
        let env = Some(OsString::from("from env"));
        let password = resolve(true, &mut &b"from stdin\n"[..], env, no_prompt, not_asked).unwrap();
        assert_eq!(password.expose(), "from stdin");
    }

    #[test]
    fn the_environment_wins_over_the_prompt() {
        let env = Some(OsString::from("from env"));
        let password = resolve(false, &mut io::empty(), env, no_prompt, not_asked).unwrap();
        assert_eq!(password.expose(), "from env");
    }

    #[test]
    fn the_environment_is_taken_as_it_is() {
        // No trimming, and a variable that is set to nothing is an explicit
        // empty password.
        let env = Some(OsString::from(" spaced\n"));
        assert_eq!(
            resolve(false, &mut io::empty(), env, no_prompt, not_asked)
                .unwrap()
                .expose(),
            " spaced\n"
        );
        let env = Some(OsString::new());
        assert_eq!(
            resolve(false, &mut io::empty(), env, no_prompt, not_asked)
                .unwrap()
                .expose(),
            ""
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_environment_value_that_is_not_text_is_rejected() {
        use std::os::unix::ffi::OsStringExt;

        let env = Some(OsString::from_vec(vec![0xff, 0xfe]));
        let result = resolve(false, &mut io::empty(), env, no_prompt, not_asked);
        assert!(matches!(result, Err(PasswordError::NotUtf8(_))));
    }

    #[test]
    fn the_prompt_comes_before_the_program_that_asks() {
        let password = resolve(
            false,
            &mut io::empty(),
            None,
            || Prompted::Password("typed".to_owned()),
            not_asked,
        )
        .unwrap();
        assert_eq!(password.expose(), "typed");
        // Enter alone is an explicit empty password.
        let password = resolve(
            false,
            &mut io::empty(),
            None,
            || Prompted::Password(String::new()),
            not_asked,
        )
        .unwrap();
        assert_eq!(password.expose(), "");
    }

    #[test]
    fn without_any_source_the_error_names_all_four() {
        let error = resolve(false, &mut io::empty(), None, no_terminal, no_program).unwrap_err();
        assert!(matches!(error, PasswordError::Unavailable(_)));

        let message = Error::from(error).to_string();
        assert!(message.contains("--password-stdin"), "{message}");
        assert!(message.contains("B4_PASSWORD"), "{message}");
        assert!(message.contains("terminal"), "{message}");
        assert!(message.contains("B4_ASKPASS"), "{message}");
        // The operating system's reason is not part of the message.
        assert!(
            !message.to_lowercase().contains("no such device"),
            "{message}"
        );
    }

    #[test]
    fn stdin_without_the_flag_is_not_read() {
        let mut input: &[u8] = b"not a password\n";
        let error = resolve(false, &mut input, None, no_terminal, no_program).unwrap_err();
        assert!(matches!(error, PasswordError::Unavailable(_)));
        assert_eq!(input, b"not a password\n");
    }

    /// The variable the two tests below use. Not [`ENV_VAR`]: the tests of
    /// this binary run side by side in one process.
    const TEST_VAR: &str = "B4_TEST_TAKE_ENV";

    /// Runs alone in a process of its own, started by the test below with
    /// the variable set: the environment is only changed while nothing else
    /// reads it.
    #[test]
    #[ignore = "started by taking_the_variable_removes_it_from_the_process"]
    fn take_env_in_a_process_of_its_own() {
        if std::env::var_os(TEST_VAR).is_none() {
            // Run by hand with --ignored, without the variable: nothing to
            // check.
            return;
        }
        let value = take_env(TEST_VAR).expect("the variable was set");
        assert_eq!(value, "s3cret from the environment");
        // Gone for the rest of the process ...
        assert_eq!(std::env::var_os(TEST_VAR), None);
        assert!(std::env::vars_os().all(|(name, _)| name != TEST_VAR));
        // ... a second call finds nothing ...
        assert_eq!(take_env(TEST_VAR), None);
        // ... and a process started now does not inherit it.
        #[cfg(unix)]
        {
            let child = std::process::Command::new("sh")
                .args(["-c", "printf %s \"${B4_TEST_TAKE_ENV-not inherited}\""])
                .output()
                .expect("cannot run sh");
            assert_eq!(String::from_utf8_lossy(&child.stdout), "not inherited");
        }
    }

    #[test]
    fn taking_the_variable_removes_it_from_the_process() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "password::tests::take_env_in_a_process_of_its_own",
                "--ignored",
                "--test-threads=1",
            ])
            .env(TEST_VAR, "s3cret from the environment")
            .output()
            .expect("cannot run the test binary");
        let report = String::from_utf8_lossy(&output.stdout);
        assert!(output.status.success(), "{report}");
        assert!(report.contains("1 passed"), "{report}");
        // A variable that is not set is nothing, and nothing is changed.
        assert_eq!(take_env("B4_TEST_TAKE_ENV_NEVER_SET"), None);
    }

    #[test]
    fn debug_does_not_show_the_password() {
        let password = Password::new("hunter2".to_owned());
        assert_eq!(format!("{password:?}"), "Password(<redacted>)");
    }

    #[test]
    fn errors_do_not_show_the_password() {
        let error = from_stdin(b"hunter2\xff\n").unwrap_err();
        assert!(!Error::from(error).to_string().contains("hunter2"));
    }

    // --- the program that asks ---

    #[test]
    fn without_a_terminal_a_program_is_asked() {
        let asked = || Some(Ok(Password::new("from the dialog".to_owned())));
        let password = resolve(false, &mut io::empty(), None, no_terminal, asked).unwrap();
        assert_eq!(password.expose(), "from the dialog");
    }

    #[test]
    fn a_cancelled_dialog_is_the_error_and_not_no_password_available() {
        let cancelled = || {
            Some(Err(PasswordError::AskpassTooLong {
                program: "ask".to_owned(),
            }))
        };
        let error = resolve(false, &mut io::empty(), None, no_terminal, cancelled).unwrap_err();
        assert!(matches!(error, PasswordError::AskpassTooLong { .. }));
    }

    #[test]
    fn stdin_and_the_environment_come_before_the_program_that_asks() {
        // `not_asked` panics when it is reached.
        let password = resolve(
            true,
            &mut &b"from stdin\n"[..],
            None,
            no_terminal,
            not_asked,
        );
        assert_eq!(password.unwrap().expose(), "from stdin");
        let env = Some(OsString::from("from env"));
        let password = resolve(false, &mut io::empty(), env, no_terminal, not_asked);
        assert_eq!(password.unwrap().expose(), "from env");
    }

    #[test]
    fn a_prompt_that_was_cancelled_is_an_answer() {
        // There is a terminal and the user backed out there: Ctrl-D, or
        // Ctrl-C where it does not end b4. No dialog follows (`not_asked`
        // panics when it is reached).
        for kind in [io::ErrorKind::UnexpectedEof, io::ErrorKind::Interrupted] {
            let cancelled = || Prompted::Failed(io::Error::from(kind));
            let error = resolve(false, &mut io::empty(), None, cancelled, not_asked).unwrap_err();
            assert!(matches!(error, PasswordError::PromptCancelled), "{error:?}");
            let message = Error::from(error).to_string();
            assert!(message.contains("cancelled"), "{message}");
        }
        // Nor after any other failure of a terminal that is there.
        let broken = || Prompted::Failed(io::Error::from_raw_os_error(5));
        let error = resolve(false, &mut io::empty(), None, broken, not_asked).unwrap_err();
        assert!(matches!(error, PasswordError::PromptFailed(_)), "{error:?}");
    }

    /// An environment for [`askpass_program`].
    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    }

    #[test]
    fn the_program_is_b4s_own_variable_or_in_a_graphical_session_that_of_ssh() {
        let chosen = |pairs| {
            askpass_program(env_of(pairs))
                .map(|(program, variable)| (program.into_string().unwrap(), variable))
        };

        // Its own variable always counts, also without a display, and wins.
        assert_eq!(
            chosen(&[("B4_ASKPASS", "/bin/mine")]),
            Some(("/bin/mine".to_owned(), "B4_ASKPASS"))
        );
        assert_eq!(
            chosen(&[
                ("B4_ASKPASS", "/bin/mine"),
                ("SSH_ASKPASS", "/bin/ssh-ask"),
                ("DISPLAY", ":0"),
            ]),
            Some(("/bin/mine".to_owned(), "B4_ASKPASS"))
        );

        // That of ssh only where a dialog can appear.
        assert_eq!(
            chosen(&[("SSH_ASKPASS", "/bin/ssh-ask"), ("DISPLAY", ":0")]),
            Some(("/bin/ssh-ask".to_owned(), "SSH_ASKPASS"))
        );
        assert_eq!(
            chosen(&[
                ("SSH_ASKPASS", "/bin/ssh-ask"),
                ("WAYLAND_DISPLAY", "wayland-1")
            ]),
            Some(("/bin/ssh-ask".to_owned(), "SSH_ASKPASS"))
        );
        assert_eq!(chosen(&[("SSH_ASKPASS", "/bin/ssh-ask")]), None);

        // A variable that is set to nothing is not set.
        assert_eq!(
            chosen(&[
                ("B4_ASKPASS", ""),
                ("SSH_ASKPASS", "/bin/ssh-ask"),
                ("DISPLAY", "")
            ]),
            None
        );
        assert_eq!(chosen(&[("DISPLAY", ":0")]), None);
        assert_eq!(chosen(&[]), None);

        // Who tells ssh never to use its program gets none from b4 either;
        // b4's own variable is not affected.
        assert_eq!(
            chosen(&[
                ("SSH_ASKPASS", "/bin/ssh-ask"),
                ("DISPLAY", ":0"),
                ("SSH_ASKPASS_REQUIRE", "never"),
            ]),
            None
        );
        assert_eq!(
            chosen(&[
                ("SSH_ASKPASS", "/bin/ssh-ask"),
                ("DISPLAY", ":0"),
                ("SSH_ASKPASS_REQUIRE", "prefer"),
            ]),
            Some(("/bin/ssh-ask".to_owned(), "SSH_ASKPASS"))
        );
        assert_eq!(
            chosen(&[
                ("B4_ASKPASS", "/bin/mine"),
                ("SSH_ASKPASS_REQUIRE", "never")
            ]),
            Some(("/bin/mine".to_owned(), "B4_ASKPASS"))
        );
    }

    #[test]
    fn the_question_names_b4_and_the_vault_file() {
        assert_eq!(
            question(Path::new("/home/erika/Banking/Haushaltsbuch.sub")),
            "b4: password for the Banking4 vault \"Haushaltsbuch.sub\": "
        );
        // Control characters would be shown by a dialog as they are.
        assert_eq!(
            question(Path::new("evil\u{1b}[31m\nname.sub")),
            "b4: password for the Banking4 vault \"evil?[31m?name.sub\": "
        );
        // Whatever the file is called, the question begins and ends the same
        // way: a name cannot make it read like a question of another program.
        for name in [
            "x Verification code",
            "Enter PIN for 'x'",
            "a@b's password",
            "Password for 'https://example.org'",
            "quote\" Verification code",
            "",
        ] {
            let asked = question(Path::new(name));
            assert!(
                asked.starts_with("b4: password for the Banking4 vault \""),
                "{asked}"
            );
            assert!(asked.ends_with("\": "), "{asked}");
        }
        // So would the overrides of the text direction, and a line separator
        // would start a line of its own in the dialog.
        assert_eq!(
            question(Path::new("a\u{2028}Login password\u{202e}b.sub")),
            "b4: password for the Banking4 vault \"a?Login password?b.sub\": "
        );
        // A long name is cut.
        let long = "x".repeat(500);
        let asked = question(Path::new(&long));
        assert_eq!(asked.matches('x').count(), MAX_NAME_CHARS);
    }

    #[cfg(unix)]
    mod program {
        use super::*;

        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        /// A shell script in a directory of its own, to be run as the program
        /// that asks.
        struct Script {
            dir: tempfile::TempDir,
        }

        impl Script {
            fn new(body: &str) -> Script {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("ask");
                fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
                Script { dir }
            }

            fn ask(&self) -> Result<Password, PasswordError> {
                let path = self.dir.path().join("ask");
                // A script that was just written can be "busy" for a moment:
                // a test on another thread that starts a process at the same
                // time holds the file open for writing until its exec.
                for _ in 0..200 {
                    match run_askpass(path.as_os_str(), "B4_ASKPASS", "the question: ") {
                        Err(PasswordError::AskpassStart { source, .. })
                            if source.kind() == io::ErrorKind::ExecutableFileBusy =>
                        {
                            std::thread::sleep(std::time::Duration::from_millis(10));
                        }
                        result => return result,
                    }
                }
                panic!("the script stayed busy");
            }
        }

        fn asked(body: &str) -> Result<Password, PasswordError> {
            Script::new(body).ask()
        }

        #[test]
        fn its_first_line_is_the_password() {
            assert_eq!(asked("echo secret").unwrap().expose(), "secret");
            assert_eq!(asked("printf 'secret'").unwrap().expose(), "secret");
            assert_eq!(asked("printf 'secret\\r\\n'").unwrap().expose(), "secret");
            assert_eq!(
                asked("printf 'secret\\nsecond line\\n'").unwrap().expose(),
                "secret"
            );
            assert_eq!(
                asked("echo ' pässwörd € '").unwrap().expose(),
                " pässwörd € "
            );
        }

        #[test]
        fn an_empty_line_is_an_empty_password_and_nothing_is_not() {
            assert_eq!(asked("echo").unwrap().expose(), "");
            assert_eq!(asked("printf '\\r\\n'").unwrap().expose(), "");

            // Status 0 without a single byte: a dialog that never opened, or
            // a script around one that lost its status.
            let error = asked("true").unwrap_err();
            assert!(
                matches!(error, PasswordError::AskpassEmpty { .. }),
                "{error:?}"
            );
            let message = Error::from(error).to_string();
            assert!(message.contains("printed nothing"), "{message}");
            assert!(message.contains("B4_ASKPASS"), "{message}");
        }

        #[test]
        fn it_gets_the_question_as_its_only_argument_and_no_input() {
            assert_eq!(
                asked("printf '%s|%s' \"$#\" \"$1\"").unwrap().expose(),
                "1|the question: "
            );
            // Standard input is empty: `cat` ends at once.
            assert_eq!(asked("cat; echo done").unwrap().expose(), "done");
        }

        #[test]
        fn another_status_than_0_means_cancelled() {
            let error = asked("echo half-typed; exit 1").unwrap_err();
            assert!(matches!(error, PasswordError::AskpassFailed { .. }));
            let message = Error::from(error).to_string();
            assert!(message.contains("B4_ASKPASS"), "{message}");
            assert!(message.contains("cancelled or failed"), "{message}");
            // What the program printed is not a password and is not shown.
            assert!(!message.contains("half-typed"), "{message}");
        }

        #[test]
        fn a_program_that_does_not_exist_is_named() {
            let error =
                run_askpass(OsStr::new("/nonexistent/ask"), "SSH_ASKPASS", "?").unwrap_err();
            assert!(matches!(error, PasswordError::AskpassStart { .. }));
            let message = Error::from(error).to_string();
            assert!(message.contains("/nonexistent/ask"), "{message}");
            assert!(message.contains("SSH_ASKPASS"), "{message}");
        }

        #[test]
        fn what_it_prints_is_bounded() {
            let longest = format!("printf '%s' {}", "x".repeat(MAX_STDIN_BYTES));
            assert_eq!(asked(&longest).unwrap().expose().len(), MAX_STDIN_BYTES);

            let too_long = format!("printf '%s' {}", "x".repeat(MAX_STDIN_BYTES + 1));
            assert!(matches!(
                asked(&too_long),
                Err(PasswordError::AskpassTooLong { .. })
            ));

            // A program that never stops printing does not hang b4: it ends
            // on the closed pipe, which is a failure like any other.
            let endless = asked("while :; do printf xxxxxxxxxxxxxxxx; done").unwrap_err();
            assert!(
                matches!(
                    endless,
                    PasswordError::AskpassFailed { .. } | PasswordError::AskpassTooLong { .. }
                ),
                "{endless:?}"
            );
        }

        #[test]
        fn bytes_that_are_not_text_are_rejected() {
            let error = asked("printf '\\377\\376\\n'").unwrap_err();
            assert!(
                matches!(error, PasswordError::AskpassNotUtf8 { .. }),
                "{error:?}"
            );
            // The message names the program, which may be one the user never
            // named for b4.
            assert!(Error::from(error).to_string().contains("/ask"));
        }
    }
}
