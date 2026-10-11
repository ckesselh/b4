//! `b4 sql <QUERY>`: run a read-only SQL query against the database.

use std::io::Read;

use crate::cli::{Globals, SqlArgs};
use crate::db::read::{is_file_system_refusal, is_refusal, Database, CHUNK_ROWS};
use crate::error::Context;
use crate::output::TableStream;
use crate::Error;

/// The line added to the database's message when the sandbox refused the
/// statement.
const READ_ONLY_NOTE: &str =
    "b4 sql is read-only: it cannot change the database or reach files or the network";

/// The line added when a query may have failed for want of memory. DuckDB
/// reports that in two ways: as "Out of Memory Error", with advice to change
/// settings that are locked here, or, when it tried to spill to disk first,
/// as a refused access to the file system, which is also what a query gets
/// that reaches for a file. In the second case the line follows
/// [`READ_ONLY_NOTE`].
const MEMORY_NOTE: &str = "b4 sql gives a query 1 GiB of memory and no disk to spill to, and \
     these limits cannot be changed: reduce the query with a WHERE or a LIMIT";

/// How DuckDB's message starts when a query ran out of memory.
const OUT_OF_MEMORY: &str = "Out of Memory Error";

/// The longest query read from standard input, in bytes. A megabyte is far
/// beyond any query written by a person or a program; the limit only keeps
/// a mistaken pipe (`b4 sql - < dump.csv`) from being read into memory.
const MAX_QUERY_BYTES: u64 = 1024 * 1024;

/// Run the query `args.query` and print its result.
///
/// The query is read from standard input when the argument is `-`. The
/// result is printed as a table whose columns are the columns of the result:
/// the name as key and title, the kind from the DuckDB type (integers,
/// `DECIMAL` as a decimal column with the type's scale, `DOUBLE` as float,
/// `DATE`, `TIMESTAMP`, `BOOLEAN`, everything else as text). Repeated or
/// empty column names get distinct keys (`sum`, `sum_2`), or JSON output
/// would hold duplicate keys.
///
/// The text may hold several statements separated by `;`. They run in
/// order and the result of the last one is printed, so a query can prepare
/// itself with temporary tables or macros, which live in memory and go
/// with the process. A statement without a result prints nothing.
///
/// The connection is read-only and sandboxed from the moment it is opened
/// (see [`crate::db::read`]): a query handed to b4 by an agent can only read
/// this database. An error of the database (syntax, unknown table) is a
/// runtime error with the database's own message; a statement the sandbox
/// refused gets one more line that says why, and so does a query that ran
/// out of memory. DuckDB's switches that are
/// functions and not settings (`PRAGMA enable_profiling`, `enable_logging`
/// and the like) and the table function `query` are refused by name before
/// the statement reaches the database.
///
/// # Memory
///
/// The result is never held as a whole where that can be avoided. DuckDB
/// streams it, within the memory limit of the sandbox
/// ([`MEMORY_LIMIT_BYTES`](crate::db::read::MEMORY_LIMIT_BYTES)), and `csv`,
/// `jsonl` and `json` are written [`CHUNK_ROWS`] rows at a time. The `table`
/// format has to see every row to size its columns: it buffers up to
/// [`MAX_TABLE_ROWS`](crate::output::MAX_TABLE_ROWS) rows and fails beyond,
/// before anything is printed, with the advice to use another format or a
/// `LIMIT`. A query read from standard input may be up to
/// [`MAX_QUERY_BYTES`] long.
///
/// Because the machine formats are written while the result arrives, a query
/// that fails after its first rows (a value that cannot be cast, say) has
/// printed those rows already; the exit status 1 says that the output is
/// incomplete.
pub fn run(args: &SqlArgs, globals: &Globals) -> Result<(), Error> {
    let query = if args.reads_stdin() {
        read_query(&mut std::io::stdin().lock())?
    } else {
        args.query.clone()
    };

    let database = Database::open(&globals.db)?;
    let mut stream = None;
    database
        .for_each_chunk(&query, &[], &[], CHUNK_ROWS, |chunk| {
            if chunk.columns().is_empty() {
                // A statement without a result, such as `create temp macro`.
                return Ok(());
            }
            stream
                .get_or_insert_with(|| TableStream::stdout(globals))
                .write(chunk)
        })
        .map_err(explain_refusal)?;
    match stream {
        Some(stream) => stream.finish().map(|_| ()),
        None => Ok(()),
    }
}

/// The query on `input`, which may be at most [`MAX_QUERY_BYTES`] long.
fn read_query(input: &mut impl Read) -> Result<String, Error> {
    let mut text = String::new();
    // One byte more than allowed shows a query that is too long.
    input
        .take(MAX_QUERY_BYTES + 1)
        .read_to_string(&mut text)
        .context("cannot read the query from standard input")?;
    if text.len() as u64 > MAX_QUERY_BYTES {
        return Err(Error::new(format!(
            "the query on standard input is longer than {} MiB",
            MAX_QUERY_BYTES / (1024 * 1024)
        )));
    }
    Ok(text)
}

/// Add to the error of a statement the sandbox refused why it was refused,
/// and to one that ran out of memory what the limit is.
fn explain_refusal(error: Error) -> Error {
    let message = error.to_string();
    if is_file_system_refusal(&message) {
        Error::new(format!("{message}\n{READ_ONLY_NOTE}\n{MEMORY_NOTE}"))
    } else if message.starts_with(OUT_OF_MEMORY) {
        Error::new(format!("{message}\n{MEMORY_NOTE}"))
    } else if is_refusal(&message) {
        Error::new(format!("{message}\n{READ_ONLY_NOTE}"))
    } else {
        error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_on_standard_input_is_bounded() {
        assert_eq!(
            read_query(&mut "select 1\n".as_bytes()).unwrap(),
            "select 1\n"
        );
        assert_eq!(read_query(&mut "".as_bytes()).unwrap(), "");

        let limit = usize::try_from(MAX_QUERY_BYTES).unwrap();
        let longest = " ".repeat(limit);
        assert_eq!(read_query(&mut longest.as_bytes()).unwrap().len(), limit);
        let too_long = " ".repeat(limit + 1);
        assert_eq!(
            read_query(&mut too_long.as_bytes())
                .unwrap_err()
                .to_string(),
            "the query on standard input is longer than 1 MiB"
        );
        // Endless input is not read to its end.
        let error = read_query(&mut std::io::repeat(b'x')).unwrap_err();
        assert!(error.to_string().contains("longer than"), "{error}");

        let error = read_query(&mut &b"select '\xff'"[..]).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("cannot read the query from standard input: "),
            "{error}"
        );
    }

    #[test]
    fn a_refusal_is_explained() {
        let explained = |message: &str| explain_refusal(Error::new(message)).to_string();
        assert_eq!(
            explained("Parser Error: syntax error at end of input"),
            "Parser Error: syntax error at end of input"
        );
        assert_eq!(
            explained("Invalid Input Error: Cannot execute statement of type \"INSERT\" on database \"b4\" which is attached in read-only mode!"),
            format!("Invalid Input Error: Cannot execute statement of type \"INSERT\" on database \"b4\" which is attached in read-only mode!\n{READ_ONLY_NOTE}")
        );
        let no_memory = "Out of Memory Error: failed to allocate data of size 16.0 MiB";
        assert_eq!(explained(no_memory), format!("{no_memory}\n{MEMORY_NOTE}"));
        // Without a file system DuckDB can word "out of memory" the same
        // way as a refused file, so the user is told both.
        assert_eq!(
            crate::db::read::MEMORY_LIMIT_BYTES,
            1 << 30,
            "MEMORY_NOTE says 1 GiB"
        );
        let no_files =
            "Permission Error: File system LocalFileSystem has been disabled by configuration";
        assert_eq!(
            explained(no_files),
            format!("{no_files}\n{READ_ONLY_NOTE}\n{MEMORY_NOTE}")
        );
    }
}
