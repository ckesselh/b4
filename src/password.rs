//! Where the vault password comes from.
//!
//! There are three sources, tried in this order:
//!
//! 1. `--password-stdin`: the first line of standard input;
//! 2. the environment variable `B4_PASSWORD`;
//! 3. a prompt without echo on the controlling terminal.
//!
//! The password is never a command-line argument, because arguments are
//! visible to every process on the machine and end up in the shell history,
//! and it is never stored.
//!
//! # The environment variable
//!
//! The variable is the weakest of the three sources, and the help of
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
//! input, a variable that is set to nothing, Enter at the prompt. Standard
//! input that ends without a single byte is not a password, it is a pipeline
//! whose left side failed.

use std::ffi::OsString;
use std::fmt;
use std::io::{self, BufRead, Read};

use zeroize::{Zeroize, Zeroizing};

use crate::error::Error;

/// The environment variable that can hold the password.
pub const ENV_VAR: &str = "B4_PASSWORD";

/// The longest password accepted on standard input, in bytes. It bounds what
/// is read when standard input is not a password at all.
const MAX_STDIN_BYTES: usize = 4096;

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

    /// None of the three sources is available.
    #[error(
        "no password available: pipe it to --password-stdin, set the environment \
         variable {ENV_VAR}, or run b4 in a terminal to be asked for it"
    )]
    Unavailable(#[source] io::Error),
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

/// Obtain the vault password. `from_stdin` is the `--password-stdin` flag.
///
/// The environment variable [`ENV_VAR`] is removed from the environment of
/// the process, whether or not the password comes from it.
///
/// Call this before the process has a second thread, which for b4 means
/// before the database is opened (DuckDB starts worker threads); see
/// [`take_env`].
pub fn obtain(from_stdin: bool) -> Result<Password, Error> {
    let password = resolve(
        from_stdin,
        &mut io::stdin().lock(),
        take_env(ENV_VAR),
        || rpassword::prompt_password(PROMPT),
    )?;
    Ok(password)
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
    prompt: impl FnOnce() -> io::Result<String>,
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
    // The prompt opens the controlling terminal itself, so it works while
    // stdin or stdout are redirected, and fails when there is no terminal.
    prompt()
        .map(Password::new)
        .map_err(PasswordError::Unavailable)
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

    fn no_prompt() -> io::Result<String> {
        panic!("the prompt must not be reached")
    }

    fn no_terminal() -> io::Result<String> {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no such device or address",
        ))
    }

    fn from_stdin(input: &[u8]) -> Result<Password, PasswordError> {
        resolve(true, &mut &*input, None, no_prompt)
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
        let password = resolve(true, &mut input, None, no_prompt).unwrap();
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
        let result = resolve(true, &mut reader, None, no_prompt);
        assert!(matches!(result, Err(PasswordError::StdinTooLong)));
    }

    #[test]
    fn stdin_wins_over_the_environment() {
        let env = Some(OsString::from("from env"));
        let password = resolve(true, &mut &b"from stdin\n"[..], env, no_prompt).unwrap();
        assert_eq!(password.expose(), "from stdin");
    }

    #[test]
    fn the_environment_wins_over_the_prompt() {
        let env = Some(OsString::from("from env"));
        let password = resolve(false, &mut io::empty(), env, no_prompt).unwrap();
        assert_eq!(password.expose(), "from env");
    }

    #[test]
    fn the_environment_is_taken_as_it_is() {
        // No trimming, and a variable that is set to nothing is an explicit
        // empty password.
        let env = Some(OsString::from(" spaced\n"));
        assert_eq!(
            resolve(false, &mut io::empty(), env, no_prompt)
                .unwrap()
                .expose(),
            " spaced\n"
        );
        let env = Some(OsString::new());
        assert_eq!(
            resolve(false, &mut io::empty(), env, no_prompt)
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
        let result = resolve(false, &mut io::empty(), env, no_prompt);
        assert!(matches!(result, Err(PasswordError::NotUtf8(_))));
    }

    #[test]
    fn the_prompt_is_the_last_resort() {
        let password = resolve(false, &mut io::empty(), None, || Ok("typed".to_owned())).unwrap();
        assert_eq!(password.expose(), "typed");
        // Enter alone is an explicit empty password.
        let password = resolve(false, &mut io::empty(), None, || Ok(String::new())).unwrap();
        assert_eq!(password.expose(), "");
    }

    #[test]
    fn without_any_source_the_error_names_all_three() {
        let error = resolve(false, &mut io::empty(), None, no_terminal).unwrap_err();
        assert!(matches!(error, PasswordError::Unavailable(_)));

        let message = Error::from(error).to_string();
        assert!(message.contains("--password-stdin"), "{message}");
        assert!(message.contains("B4_PASSWORD"), "{message}");
        assert!(message.contains("terminal"), "{message}");
        // The operating system's reason is not part of the message.
        assert!(!message.contains("no such device"), "{message}");
    }

    #[test]
    fn stdin_without_the_flag_is_not_read() {
        let mut input: &[u8] = b"not a password\n";
        let error = resolve(false, &mut input, None, no_terminal).unwrap_err();
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
}
