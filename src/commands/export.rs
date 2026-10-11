//! `b4 export <DIR>`: write the tables of the database to files.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};

use crate::cli::{ExportArgs, ExportFormat, Format, Globals};
use crate::db::read::{quote_identifier, ColumnInfo, Database, Relation, CHUNK_ROWS};
use crate::error::Context;
use crate::output::{self, Options};
use crate::Error;

/// Columns that identify a row, in the order they sort by.
const KEY_COLUMNS: [&str; 2] = ["id", "account_id"];

/// How often another name is tried for the temporary file.
const TEMP_NAME_ATTEMPTS: u32 = 16;

/// Export tables into the directory `args.dir`, one file per table.
///
/// Without `--table`, every table of the database is written, `meta` and
/// `warnings` included, but no view: a view repeats rows of its tables.
/// With `--table`, exactly the named tables are written; a view can be named
/// too. An unknown name is an error that lists the tables there are, before
/// anything is written.
///
/// The directory is created if it is missing. Each file is
/// `<table>.<extension of the format>`, written by b4's own csv and jsonl
/// renderers, not by DuckDB extensions: b4 never loads one. Every column of
/// a table is exported, in table order, and the files are data: byte-exact,
/// with nothing replaced for a terminal. `--no-header` applies to csv.
///
/// The format is `--format`, or the global `--output` when that is `csv` or
/// `jsonl`, or csv. `--output` is never ignored silently: `table` or `json`
/// given on the command line, and an `--output` that contradicts
/// `--format`, are usage errors (see [`file_format`]).
///
/// A table name becomes a file name, so a name that could leave the
/// directory or that a file system reserves is refused before anything is
/// written (see [`check_file_name`]).
///
/// A file is written under a temporary name in the same directory and then
/// renamed, so an existing file is replaced in one step and a reader never
/// sees half a file. On Unix the files are created with mode 0600 and a
/// directory this command creates with mode 0700: the content is bank data.
///
/// Rows are written in a fixed order, by `id` and `account_id` where the
/// table has them and then by every other column, so that two exports of
/// the same database are the same bytes.
///
/// One line per file goes to standard error, unless `--quiet` is given.
pub fn run(args: &ExportArgs, globals: &Globals) -> Result<(), Error> {
    let format = file_format(args.format, globals.output)?;
    let database = Database::open(&globals.db)?;
    let relations = selection(&database, &args.tables)?;
    for relation in &relations {
        check_file_name(&relation.name)?;
    }

    create_directory(&args.dir)?;
    for relation in &relations {
        let path = args
            .dir
            .join(format!("{}.{}", relation.name, format.extension()));
        let rows = export_relation(&database, relation, &path, format, globals)
            .context(format!("cannot export {}", relation.name))?;
        let noun = if rows == 1 { "row" } else { "rows" };
        output::note(globals, &format!("{}: {rows} {noun}", path.display()));
    }
    Ok(())
}

/// The file format of an export: `--format` (`format`) if given, else the
/// global `--output` (`output`) when that is a format export can write, else
/// csv.
///
/// `--output` is a global option and so accepted by `b4 export` too. It must
/// not be ignored silently: `json` and `table` are usage errors that name
/// `--format`, and so is an `--output` that contradicts `--format`.
/// `output` is `None` when `--output` was not given, which is not the same
/// as a `table` that was asked for.
fn file_format(
    format: Option<ExportFormat>,
    output: Option<Format>,
) -> Result<ExportFormat, Error> {
    let name = |format: ExportFormat| format.extension();
    let asked = match output {
        None => None,
        Some(Format::Csv) => Some(ExportFormat::Csv),
        Some(Format::Jsonl) => Some(ExportFormat::Jsonl),
        Some(output @ (Format::Table | Format::Json)) => {
            let given = if output == Format::Json {
                "json"
            } else {
                "table"
            };
            return Err(Error::usage(format!(
                "b4 export cannot write `--output {given}`: choose the file format with \
                 `--format csv` or `--format jsonl`"
            )));
        }
    };
    match (format, asked) {
        (Some(format), Some(asked)) if format != asked => Err(Error::usage(format!(
            "`--format {}` and `--output {}` contradict each other: choose the file format \
             with `--format` alone",
            name(format),
            name(asked)
        ))),
        (Some(format), _) | (None, Some(format)) => Ok(format),
        (None, None) => Ok(ExportFormat::Csv),
    }
}

/// The tables to export: all of them, or the ones named, each once and in
/// the order given.
fn selection(database: &Database, names: &[String]) -> Result<Vec<Relation>, Error> {
    if names.is_empty() {
        return database.tables();
    }
    let mut selected: Vec<Relation> = Vec::with_capacity(names.len());
    for name in names {
        let relation = database.relation(name)?;
        if !selected.contains(&relation) {
            selected.push(relation);
        }
    }
    Ok(selected)
}

/// The device names Windows reserves in every directory, with or without an
/// extension and in any case of letters. The digit of `COM` and `LPT` may
/// also be one of the superscripts ¹, ² and ³.
const RESERVED_DEVICES: [&str; 4] = ["CON", "PRN", "AUX", "NUL"];
const RESERVED_NUMBERED_DEVICES: [&str; 2] = ["COM", "LPT"];

/// Whether `name`, or `name` with any extension, is a device on Windows and
/// not a file: `NUL`, `con.csv`, `COM1`, `lpt9.tar.gz`.
fn is_reserved_device(name: &str) -> bool {
    // Windows looks at the part before the first dot, without the blanks
    // at its end.
    let stem = name.split('.').next().unwrap_or(name).trim_end_matches(' ');
    let stem = stem.to_uppercase();
    RESERVED_DEVICES.contains(&stem.as_str())
        || RESERVED_NUMBERED_DEVICES.iter().any(|device| {
            let mut rest = stem.strip_prefix(device).unwrap_or("").chars();
            matches!(
                (rest.next(), rest.next()),
                (Some('0'..='9' | '¹' | '²' | '³'), None)
            )
        })
}

/// A table name becomes a file name. b4's own tables are plain words; a
/// database from elsewhere must not be able to write outside the directory,
/// on any system. Refused are:
///
/// * the empty name and a name that starts with a dot (`..`, hidden files);
/// * `/`, `\` and NUL, the separators and the terminator of paths;
/// * `:`, which on Windows names a drive (`C:notes` is a file in the current
///   directory of drive C, not in the export directory) or a stream of
///   another file;
/// * control characters;
/// * the reserved device names of Windows ([`is_reserved_device`]);
/// * a name that ends in a dot or a blank, which Windows strips, so that
///   the file would get another name than the table.
///
/// The rules are the same on every system: an export is often made on one
/// machine and unpacked on another.
fn check_file_name(name: &str) -> Result<(), Error> {
    let unsafe_name = name.is_empty()
        || name.starts_with('.')
        || name.ends_with(['.', ' '])
        || name.contains(['/', '\\', '\0', ':'])
        || name.chars().any(char::is_control)
        || is_reserved_device(name);
    if unsafe_name {
        return Err(Error::new(format!(
            "cannot export the table `{}`: its name cannot be used as a file name",
            name.escape_default()
        )));
    }
    Ok(())
}

fn create_directory(dir: &Path) -> Result<(), Error> {
    if dir.is_dir() {
        return Ok(());
    }
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .context(format!("cannot create the directory {}", dir.display()))
}

/// Write `relation` to `path` and return the number of rows.
fn export_relation(
    database: &Database,
    relation: &Relation,
    path: &Path,
    format: ExportFormat,
    globals: &Globals,
) -> Result<u64, Error> {
    let columns = database.columns(relation)?;
    let sql = format!(
        "select * from {} order by {}",
        quote_identifier(&relation.name),
        order_by(&columns)
    );
    let mut options = Options {
        format: match format {
            ExportFormat::Csv => Format::Csv,
            ExportFormat::Jsonl => Format::Jsonl,
        },
        header: !globals.no_header,
        width: None,
    };
    // `render_table` writes data: exact bytes, nothing replaced for a
    // terminal.

    let (temp_path, file) = create_temp_file(path)?;
    let mut out = BufWriter::new(file);
    let mut rows: u64 = 0;
    let written = database
        .for_each_chunk(&sql, &[], &[], CHUNK_ROWS, |chunk| {
            rows += chunk.len() as u64;
            output::render_table(&mut out, &chunk, &options)?;
            // The header belongs to the first chunk only.
            options.header = false;
            Ok(())
        })
        .and_then(|()| Ok(out.flush()?))
        .and_then(|()| {
            drop(out);
            fs::rename(&temp_path, path)
                .context(format!("cannot move the file to {}", path.display()))
        });
    if let Err(error) = written {
        // Nothing useful can be done if the removal fails as well.
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(rows)
}

/// The sort order of an export: the key columns the table has, then every
/// other column. Record ids can repeat (a vault may hold an id twice), so
/// only all columns together make the order total.
fn order_by(columns: &[ColumnInfo]) -> String {
    let keys = KEY_COLUMNS
        .iter()
        .filter_map(|key| columns.iter().find(|column| column.name == *key));
    let others = columns
        .iter()
        .filter(|column| !KEY_COLUMNS.contains(&column.name.as_str()));
    let terms: Vec<String> = keys
        .chain(others)
        .map(|column| quote_identifier(&column.name))
        .collect();
    if terms.is_empty() {
        // A table without columns has no rows to order.
        "1".to_owned()
    } else {
        terms.join(", ")
    }
}

/// Create a new, empty file next to `target` that only the user can read,
/// for the content that is then renamed to `target`.
fn create_temp_file(target: &Path) -> Result<(PathBuf, File), Error> {
    let directory = target.parent().unwrap_or(Path::new("."));
    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut options = OpenOptions::new();
    // create_new: never write through a file or link someone else put there.
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let process = std::process::id();
    for attempt in 0..TEMP_NAME_ATTEMPTS {
        let path = directory.join(format!(".{name}.{process}.{attempt}.tmp"));
        match options.open(&path) {
            Ok(file) => return Ok((path, file)),
            // Left behind by a process that was killed: take the next name.
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(Error::from(error)
                    .context(format!("cannot create a file in {}", directory.display())));
            }
        }
    }
    Err(Error::new(format!(
        "cannot create a temporary file in {}: remove the leftover `.{name}.*.tmp` files",
        directory.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str) -> ColumnInfo {
        ColumnInfo {
            name: name.to_owned(),
            data_type: "VARCHAR".to_owned(),
            description: None,
        }
    }

    #[test]
    fn rows_are_ordered_by_the_keys_and_then_by_everything_else() {
        let columns = [column("name"), column("account_id"), column("id")];
        assert_eq!(order_by(&columns), r#""id", "account_id", "name""#);
        assert_eq!(
            order_by(&[column("key"), column("va\"lue")]),
            r#""key", "va""lue""#
        );
        assert_eq!(order_by(&[]), "1");
    }

    #[test]
    fn only_plain_names_become_file_names() {
        for name in [
            "bookings",
            "stock prices",
            "standing_orders",
            "Überweisungen",
            "a.b",
            // Not devices: longer names, or no digit.
            "console",
            "com",
            "COM10",
            "lpt",
            "nullable",
            "aux_data",
            "com1x",
        ] {
            assert!(check_file_name(name).is_ok(), "{name:?}");
        }
        for name in [
            "",
            ".hidden",
            "..",
            "a/b",
            "a\\b",
            "a\nb",
            "a\0b",
            // A drive or a stream on Windows.
            "C:notes",
            "c:",
            "bookings:stream",
            // Devices on Windows, in any case and with any extension.
            "CON",
            "con",
            "Prn",
            "AUX",
            "nul",
            "NUL.txt",
            "con.tar.gz",
            "COM1",
            "com9",
            "COM0",
            "LPT1",
            "lpt9.csv",
            "COM¹",
            "lpt³",
            "NUL ",
            "aux .x",
            // Stripped by Windows.
            "bookings.",
            "bookings ",
            "a. ",
        ] {
            let error = check_file_name(name).unwrap_err().to_string();
            assert!(
                error.ends_with("its name cannot be used as a file name"),
                "{name:?}: {error}"
            );
            assert!(!error.contains(['\n', '\0']), "{name:?}");
        }
    }

    #[test]
    fn the_format_comes_from_format_or_from_output() {
        use ExportFormat::{Csv, Jsonl};
        // Nothing given: csv.
        assert_eq!(file_format(None, None).unwrap(), Csv);
        // --output alone selects the format when export can write it.
        assert_eq!(file_format(None, Some(Format::Jsonl)).unwrap(), Jsonl);
        assert_eq!(file_format(None, Some(Format::Csv)).unwrap(), Csv);
        // --format alone, or both in agreement.
        assert_eq!(file_format(Some(Jsonl), None).unwrap(), Jsonl);
        assert_eq!(
            file_format(Some(Jsonl), Some(Format::Jsonl)).unwrap(),
            Jsonl
        );
        assert_eq!(file_format(Some(Csv), Some(Format::Csv)).unwrap(), Csv);

        // What export cannot write is a usage error that names --format.
        for (format, output, name) in [
            (None, Format::Json, "json"),
            (Some(Csv), Format::Json, "json"),
            (None, Format::Table, "table"),
            (Some(Jsonl), Format::Table, "table"),
        ] {
            let error = file_format(format, Some(output)).unwrap_err();
            assert_eq!(error.exit_code(), 2);
            assert_eq!(
                error.to_string(),
                format!(
                    "b4 export cannot write `--output {name}`: choose the file format with \
                     `--format csv` or `--format jsonl`"
                )
            );
        }
        // And so is a contradiction.
        let error = file_format(Some(Csv), Some(Format::Jsonl)).unwrap_err();
        assert_eq!(error.exit_code(), 2);
        assert_eq!(
            error.to_string(),
            "`--format csv` and `--output jsonl` contradict each other: choose the file \
             format with `--format` alone"
        );
        assert!(file_format(Some(Jsonl), Some(Format::Csv)).is_err());
    }
}
