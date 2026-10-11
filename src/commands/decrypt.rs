//! `b4 decrypt <VAULT> [OUT]`: decrypt a vault into a DuckDB database.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::cli::{DecryptArgs, Globals};
use crate::db::write::{self, Source, Written};
use crate::db::{self, schema};
use crate::error::Context;
use crate::load::{self, Snapshot};
use crate::vault::{Vault, VaultError, MAX_VAULT_SIZE};
use crate::{output, password, Error};

/// Decrypt the vault `args.vault` into the database `args.target(globals)`
/// (OUT if given, else `--db` / `$B4_DB` / `./b4.duckdb`).
///
/// The vault is opened for reading only, read once into memory and closed;
/// nothing is ever written to it and no lock is taken. The password comes
/// from `--password-stdin`, `B4_PASSWORD`, a prompt or, without a terminal, a
/// program that asks for it, is used to unlock and dropped at once. Bank
/// access data is never read (the allow-list is in `load`).
///
/// The database is built beside the target and renamed into place when it
/// is complete, so an existing database is replaced only by a whole new one
/// and stays untouched when anything fails: this is also the refresh
/// command.
///
/// Standard error gets one line per table, the number of bank access
/// entries left unread, the warnings and where the database is; `--quiet`
/// leaves only the warnings. Nothing is written to standard output.
pub fn run(args: &DecryptArgs, globals: &Globals) -> Result<(), Error> {
    let vault_path = absolute(&args.vault);
    let target = args.target(globals);

    // Everything that can be refused without the password is refused before
    // asking for it.
    write::check_target(target, &vault_path)?;
    let bytes = read_vault(&args.vault)?;
    let checksum = db::content_checksum(&bytes);
    let size = bytes.len() as u64;
    let naming = |error: VaultError| {
        Error::from(error).context(format!("cannot decrypt {}", args.vault.display()))
    };
    let mut vault = Vault::from_bytes(bytes).map_err(naming)?;
    if vault.header().is_device_bound() {
        return Err(naming(VaultError::DeviceBound));
    }

    let password = password::obtain(args.password_stdin, &args.vault)?;
    let unlocked = vault.unlock(password.expose());
    drop(password);
    unlocked.map_err(naming)?;

    let snapshot = load::load(&vault).map_err(|error| {
        Error::from(error).context(format!("cannot decrypt {}", args.vault.display()))
    })?;
    let source = Source {
        path: &vault_path,
        header: vault.header(),
        size,
        checksum: &checksum,
    };
    let written = write::write(&snapshot, &source, target)?;
    report(&snapshot, &written, target, globals);
    Ok(())
}

/// `path` as an absolute path, without resolving links: what `b4 status`
/// looks for later, from whatever directory it is run in.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The whole vault file, read in one pass through a handle opened for
/// reading only.
fn read_vault(path: &Path) -> Result<Vec<u8>, Error> {
    let context = format!("cannot read the vault file {}", path.display());
    let file = File::open(path).context(&context)?;
    let len = file.metadata().context(&context)?.len();
    if len > MAX_VAULT_SIZE {
        return Err(Error::from(VaultError::TooLarge {
            limit: MAX_VAULT_SIZE,
        })
        .context(format!("cannot decrypt {}", path.display())));
    }
    // The length is a hint only: a file that grows while it is read is cut
    // off one byte past the limit, which the container check then refuses.
    let mut bytes = Vec::with_capacity(usize::try_from(len).unwrap_or(0));
    file.take(MAX_VAULT_SIZE + 1)
        .read_to_end(&mut bytes)
        .context(&context)?;
    Ok(bytes)
}

/// The summary on standard error.
fn report(snapshot: &Snapshot, written: &Written, target: &Path, globals: &Globals) {
    let width = schema::tables()
        .iter()
        .map(|table| table.name.len())
        .max()
        .unwrap_or(0);
    for (table, rows) in &written.rows {
        output::note(globals, &format!("{table:<width$}  {rows:>7}"));
    }
    output::note(
        globals,
        &format!(
            "bank access entries left unread: {}",
            snapshot.entries_never_read
        ),
    );
    for note in &snapshot.notes {
        output::note(globals, &format!("note: {note}"));
    }
    for warning in snapshot.warnings.iter().chain(&written.warnings) {
        output::warning(warning);
    }
    output::note(globals, &format!("database: {}", target.display()));
    let hint = if target == globals.db {
        "try: b4 accounts list".to_owned()
    } else {
        format!("try: b4 --db {} accounts list", target.display())
    };
    output::note(globals, &hint);
}
