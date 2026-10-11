//! Rendering of results as table, JSON, JSON lines and CSV.
//!
//! Every command builds one of two things and hands it to this module: a
//! [`Table`] (columns plus rows of typed, nullable cells) for lists, or a
//! [`Record`] (one row, shown as key and value) for `show` commands. The
//! format chosen with `--output` decides how it is written.
//!
//! What the formats look like:
//!
//! * `table`: aligned plain text with an upper-case header line. Numbers are
//!   right-aligned, everything else left-aligned. A money column shows the
//!   amount with at least two decimals followed by its currency, and the
//!   currency column it refers to is not shown a second time. When stdout is a
//!   terminal the table is fitted to its width by cutting the widest
//!   [flexible](Column::flexible) text columns and marking the cut with `…`;
//!   lines are never wrapped, and nothing is cut when stdout is a file or a
//!   pipe. A line break inside a cell is shown as ` ↵ `. An empty table prints
//!   nothing at all.
//! * `json`: one array with one object per row and line, keys in column order.
//!   An empty table is `[]`.
//! * `jsonl`: one object per line, nothing for an empty table.
//! * `csv`: RFC 4180 quoting with a header line of column names. Lines end in
//!   LF, not CRLF, because the output is made for Unix pipelines where a stray
//!   carriage return ends up in the last field. An empty table is the header
//!   alone. A missing value is an empty field, an empty text is `""`.
//!
//! In the three machine formats a decimal is written exactly, without trailing
//! zeros (money keeps at least two decimals). Dates are `YYYY-MM-DD`,
//! timestamps `YYYY-MM-DDTHH:MM:SS` with microseconds only when they are not
//! zero; booleans are `true` and `false`.
//!
//! # Numbers in JSON
//!
//! Many JSON readers keep every number in a double. A number is therefore
//! written as a JSON number only when a double carries it through unchanged,
//! and as a JSON string of the same digits otherwise:
//!
//! * an integer is a number when its magnitude is at most 2^53
//!   (9007199254740992), the range in which every integer is a double;
//! * a decimal is a number when it has at most 15 significant digits, which
//!   is what a double is guaranteed to carry through a round trip. An integer
//!   wider than 64 bits is a decimal without decimals and follows this rule.
//!
//! The same holds for the numbers inside the JSON text of a `LIST`, `STRUCT`
//! or `MAP` cell (see `db::read`). A float is always a number; `NaN` and the
//! infinities, which JSON cannot express, are strings.
//!
//! There is no colour in any format.
//!
//! # Text from the vault and the terminal
//!
//! The purpose of a transfer is chosen by whoever sent the money. It can
//! hold ESC and other control characters and the text direction overrides of
//! Unicode, which must not be able to move the cursor, recolour the screen or
//! make a line read differently from what it holds. These are the rules:
//!
//! * **Messages on standard error** (errors, warnings, notes) never contain a
//!   control character other than line feed and tab, nor a direction
//!   override: [`message_text`] replaces each with U+FFFD. That includes the
//!   messages of the database, which quote data.
//! * **`table`** output is made for people and is always cleaned, wherever it
//!   goes: a line break becomes ` ↵ `, a tab a blank, any other control
//!   character or direction override U+FFFD.
//! * **`csv`, `jsonl` and `json` written to a terminal** are cleaned as well.
//!   CSV replaces the same characters with U+FFFD, except the tab and the
//!   line breaks LF and CRLF, which quoting takes care of. JSON writes them
//!   as `\uXXXX` escapes, so what a terminal shows is still the exact value
//!   once it is parsed.
//! * **`csv`, `jsonl` and `json` written to a file or a pipe**, and
//!   everything `b4 export` writes, are data and stay byte-exact: nothing is
//!   replaced. (JSON escapes the characters below U+0020, as it must.)
//!
//! The renderers take the destination as a flag (`terminal`), so that both
//! cases can be tested without a terminal.
//!
//! Column widths use an approximation of the display width (see
//! [`display_width`]) instead of the Unicode tables, to stay free of another
//! dependency.

use std::fmt;
use std::io::{self, BufWriter, IsTerminal, Write};

use rust_decimal::{Decimal, RoundingStrategy};

use crate::cli::{Format, Globals};
use crate::error::Error;

/// What a line break inside a table cell is replaced with.
const LINE_BREAK: &str = " ↵ ";

/// Marks a cell that was cut to fit the terminal.
const ELLIPSIS: char = '…';

/// Blank columns between two table columns.
const GUTTER: usize = 2;

/// A flexible column is never cut below this width (or its title's width).
const MIN_FLEXIBLE_WIDTH: usize = 8;

/// Decimals shown for money when the value has fewer.
const MONEY_SCALE: u32 = 2;

/// The largest number of significant digits a double carries through a
/// round trip from and to decimal text unchanged.
pub(crate) const DOUBLE_DIGITS: usize = 15;

/// 2^53: up to this magnitude every integer is a double.
pub(crate) const MAX_SAFE_INTEGER: u64 = 1 << 53;

/// The most rows [`TableStream`] buffers for the `table` format, which needs
/// every row before it can size its columns. One hundred thousand rows of a
/// dozen columns are some tens of megabytes; nobody reads more in a terminal.
pub const MAX_TABLE_ROWS: usize = 100_000;

/// The most text, in bytes, [`TableStream`] buffers for the `table` format,
/// so that a few rows of very long text cannot exhaust memory either.
pub const MAX_TABLE_TEXT_BYTES: usize = 256 * 1024 * 1024;

/// The largest scale a 96-bit decimal can have.
const MAX_SCALE: u32 = 28;

const MICROS_PER_SECOND: i64 = 1_000_000;
const MICROS_PER_DAY: i64 = 86_400 * MICROS_PER_SECOND;

// --- Dates -------------------------------------------------------------------

/// A calendar date in the proleptic Gregorian calendar, without a time zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Date {
    year: i32,
    month: u8,
    day: u8,
}

impl Date {
    /// The date `year`-`month`-`day`, or `None` if there is no such day.
    pub fn new(year: i32, month: u8, day: u8) -> Option<Date> {
        if day == 0 || day > days_in_month(year, month) {
            return None;
        }
        Some(Date { year, month, day })
    }

    /// The date `days` days after 1970-01-01, which is how DuckDB hands out a
    /// `DATE`.
    pub fn from_days_since_epoch(days: i32) -> Date {
        civil_from_days(i64::from(days))
    }

    /// The number of days from 1970-01-01 to this date.
    pub fn days_since_epoch(self) -> i64 {
        // Days from civil, after Howard Hinnant's `days_from_civil`. The year
        // starts in March so that the leap day is the last day of the year.
        let year = i64::from(self.year) - i64::from(self.month <= 2);
        let era = year.div_euclid(400);
        let year_of_era = year.rem_euclid(400);
        let month = i64::from(self.month);
        let shifted_month = if month > 2 { month - 3 } else { month + 9 };
        let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(self.day) - 1;
        let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
        era * 146_097 + day_of_era - 719_468
    }

    /// The year.
    pub fn year(self) -> i32 {
        self.year
    }

    /// The month, 1 to 12.
    pub fn month(self) -> u8 {
        self.month
    }

    /// The day of the month, 1 to 31.
    pub fn day(self) -> u8 {
        self.day
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.year, self.month, self.day)
    }
}

/// The number of days of `month` in `year`; 0 if `month` is not 1 to 12.
pub fn days_in_month(year: i32, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Civil from days, after Howard Hinnant's `civil_from_days`. Total for every
/// day count that comes out of an `i32` of days or an `i64` of microseconds.
fn civil_from_days(days: i64) -> Date {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);

    // The casts cannot truncate: month and day are in range by construction
    // and the year of any accepted input is far inside an i32.
    Date {
        year: year.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        month: month as u8,
        day: day as u8,
    }
}

/// A date and time of day without a time zone, in microseconds since
/// 1970-01-01 00:00:00, which is how DuckDB hands out a `TIMESTAMP`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Timestamp {
    micros: i64,
}

impl Timestamp {
    /// The timestamp `micros` microseconds after 1970-01-01 00:00:00.
    pub fn from_micros(micros: i64) -> Timestamp {
        Timestamp { micros }
    }

    /// The calendar date of this timestamp.
    pub fn date(self) -> Date {
        civil_from_days(self.micros.div_euclid(MICROS_PER_DAY))
    }

    /// The text form with `separator` between date and time.
    fn text(self, separator: char) -> String {
        let in_day = self.micros.rem_euclid(MICROS_PER_DAY);
        let seconds = in_day / MICROS_PER_SECOND;
        let fraction = in_day % MICROS_PER_SECOND;
        let mut text = format!(
            "{}{separator}{:02}:{:02}:{:02}",
            self.date(),
            seconds / 3_600,
            seconds / 60 % 60,
            seconds % 60
        );
        if fraction != 0 {
            text.push_str(&format!(".{fraction:06}"));
        }
        text
    }
}

impl fmt::Display for Timestamp {
    /// ISO 8601: `YYYY-MM-DDTHH:MM:SS`, with microseconds when not zero.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text('T'))
    }
}

// --- The result model --------------------------------------------------------

/// The type of a column. It decides alignment and number formatting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Free text. Left-aligned.
    Text,
    /// A whole number. Right-aligned.
    Integer,
    /// A floating point number, as SQL aggregates like `avg` return it.
    /// Right-aligned.
    Float,
    /// An exact decimal that the table shows rounded to `scale` decimals. The
    /// machine formats write it exactly. Right-aligned.
    Decimal {
        /// Decimals shown in table output.
        scale: u32,
    },
    /// An exact amount of money. The table shows it with at least two decimals
    /// followed by the currency of the same row. Right-aligned.
    Money {
        /// Name of the column that holds the currency code.
        currency: String,
    },
    /// A calendar date. Left-aligned.
    Date,
    /// A date and time. Left-aligned.
    Timestamp,
    /// True or false. Left-aligned.
    Boolean,
}

impl Kind {
    fn is_right_aligned(&self) -> bool {
        matches!(
            self,
            Kind::Integer | Kind::Float | Kind::Decimal { .. } | Kind::Money { .. }
        )
    }
}

/// One column of a result: its key, its title and its type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Column {
    name: String,
    title: String,
    kind: Kind,
    flexible: bool,
}

impl Column {
    /// A column of the given kind. `name` is the stable snake_case key used by
    /// JSON and CSV; `title` is what a person reads in the table header and in
    /// a record.
    pub fn new(name: impl Into<String>, title: impl Into<String>, kind: Kind) -> Column {
        Column {
            name: name.into(),
            title: title.into(),
            kind,
            flexible: false,
        }
    }

    /// A text column.
    pub fn text(name: impl Into<String>, title: impl Into<String>) -> Column {
        Column::new(name, title, Kind::Text)
    }

    /// A column of whole numbers.
    pub fn integer(name: impl Into<String>, title: impl Into<String>) -> Column {
        Column::new(name, title, Kind::Integer)
    }

    /// A column of exact decimals shown with `scale` decimals in a table.
    pub fn decimal(name: impl Into<String>, title: impl Into<String>, scale: u32) -> Column {
        Column::new(name, title, Kind::Decimal { scale })
    }

    /// A column of amounts whose currency is in the column named `currency`.
    pub fn money(
        name: impl Into<String>,
        title: impl Into<String>,
        currency: impl Into<String>,
    ) -> Column {
        Column::new(
            name,
            title,
            Kind::Money {
                currency: currency.into(),
            },
        )
    }

    /// A column of booleans.
    pub fn boolean(name: impl Into<String>, title: impl Into<String>) -> Column {
        Column::new(name, title, Kind::Boolean)
    }

    /// Allow the table to cut this column when the terminal is too narrow.
    /// Only text columns are ever cut; mark the long free-text ones (purpose,
    /// notes), not the ones a reader needs in full (IBAN, name of an account).
    pub fn flexible(mut self) -> Column {
        self.flexible = true;
        self
    }

    /// The key of this column in JSON and CSV.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The title of this column for people.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The type of this column.
    pub fn kind(&self) -> &Kind {
        &self.kind
    }
}

/// One value of a result. `Null` is a missing value.
///
/// A cell is formatted by its own variant; the column's [`Kind`] adds the
/// alignment and, for decimals, the number of decimals and the currency.
#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    /// No value.
    Null,
    /// Text.
    Text(String),
    /// A whole number.
    Integer(i64),
    /// A floating point number.
    Float(f64),
    /// An exact decimal.
    Decimal(Decimal),
    /// A calendar date.
    Date(Date),
    /// A date and time.
    Timestamp(Timestamp),
    /// True or false.
    Boolean(bool),
}

impl From<&str> for Cell {
    fn from(value: &str) -> Cell {
        Cell::Text(value.to_owned())
    }
}

impl From<String> for Cell {
    fn from(value: String) -> Cell {
        Cell::Text(value)
    }
}

impl From<i64> for Cell {
    fn from(value: i64) -> Cell {
        Cell::Integer(value)
    }
}

impl From<f64> for Cell {
    fn from(value: f64) -> Cell {
        Cell::Float(value)
    }
}

impl From<Decimal> for Cell {
    fn from(value: Decimal) -> Cell {
        Cell::Decimal(value)
    }
}

impl From<Date> for Cell {
    fn from(value: Date) -> Cell {
        Cell::Date(value)
    }
}

impl From<Timestamp> for Cell {
    fn from(value: Timestamp) -> Cell {
        Cell::Timestamp(value)
    }
}

impl From<bool> for Cell {
    fn from(value: bool) -> Cell {
        Cell::Boolean(value)
    }
}

impl<T: Into<Cell>> From<Option<T>> for Cell {
    fn from(value: Option<T>) -> Cell {
        value.map_or(Cell::Null, Into::into)
    }
}

/// The result of a list command: columns and rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Cell>>,
}

impl Table {
    /// An empty table with these columns.
    pub fn new(columns: Vec<Column>) -> Table {
        Table {
            columns,
            rows: Vec::new(),
        }
    }

    /// Append a row. A row that is shorter than the column list is completed
    /// with missing values when rendered, and cells beyond the last column are
    /// ignored.
    pub fn push(&mut self, row: Vec<Cell>) {
        self.rows.push(row);
    }

    /// The columns.
    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The rows.
    pub fn rows(&self) -> &[Vec<Cell>] {
        &self.rows
    }

    /// The number of rows.
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether the table has no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The result of a `show` command: the fields of one thing, in order.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    columns: Vec<Column>,
    cells: Vec<Cell>,
}

impl Record {
    /// An empty record.
    pub fn new() -> Record {
        Record::default()
    }

    /// Append a field.
    pub fn push(&mut self, column: Column, cell: impl Into<Cell>) {
        self.columns.push(column);
        self.cells.push(cell.into());
    }

    /// The fields: each column with its value.
    pub fn fields(&self) -> impl Iterator<Item = (&Column, &Cell)> {
        self.columns.iter().zip(&self.cells)
    }
}

/// How a result is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Options {
    /// The output format.
    pub format: Format,
    /// Whether table and CSV output start with a header line.
    pub header: bool,
    /// The width to fit a table into, in terminal columns. `None` when the
    /// output is not a terminal: then nothing is cut.
    pub width: Option<usize>,
}

impl Options {
    /// The options for writing to standard output as the global command-line
    /// options ask for it. `terminal` says whether standard output is one.
    fn for_destination(globals: &Globals, terminal: bool) -> Options {
        let width = if globals.format() == Format::Table && terminal {
            // Asks stdout first. A terminal that does not tell its size gets
            // the full lines.
            terminal_size::terminal_size().map(|(width, _)| usize::from(width.0))
        } else {
            None
        };
        Options {
            format: globals.format(),
            header: !globals.no_header,
            width,
        }
    }
}

// --- Writing to the standard streams -----------------------------------------

/// Write a table to standard output in the format the user asked for.
///
/// An empty table in table format prints nothing to standard output and
/// `no rows` to standard error, unless `--quiet` was given.
pub fn print_table(table: &Table, globals: &Globals) -> Result<(), Error> {
    let terminal = io::stdout().is_terminal();
    let options = Options::for_destination(globals, terminal);
    to_stdout(|out| render_table_on(out, table, &options, terminal))?;
    if table.is_empty() && options.format == Format::Table {
        note(globals, "no rows");
    }
    Ok(())
}

/// Write a record to standard output in the format the user asked for.
pub fn print_record(record: &Record, globals: &Globals) -> Result<(), Error> {
    let terminal = io::stdout().is_terminal();
    let options = Options::for_destination(globals, terminal);
    to_stdout(|out| render_record_on(out, record, &options, terminal))
}

/// Write `bytes` to standard output as they are. For output that holds no
/// text from a vault or a database, such as a completion script; JSON goes
/// through [`print_json`].
pub fn print_bytes(bytes: &[u8]) -> Result<(), Error> {
    to_stdout(|out| out.write_all(bytes))
}

/// Write `json`, a complete JSON document a command serialised itself, to
/// standard output. On a terminal the characters a terminal acts on are
/// written as `\uXXXX` escapes (see [`json_for_terminal`]); to a file or a
/// pipe the text is written as it is.
pub fn print_json(json: &str) -> Result<(), Error> {
    if io::stdout().is_terminal() {
        let safe = json_for_terminal(json);
        to_stdout(|out| out.write_all(safe.as_bytes()))
    } else {
        to_stdout(|out| out.write_all(json.as_bytes()))
    }
}

/// Print an informational line to standard error, unless `--quiet` was given.
pub fn note(globals: &Globals, message: &str) {
    if !globals.quiet {
        // Nothing useful can be done if stderr is gone.
        let _ = writeln!(io::stderr().lock(), "{}", message_text(message));
    }
}

/// Print a warning to standard error. `--quiet` does not silence warnings.
pub fn warning(message: &str) {
    let _ = writeln!(io::stderr().lock(), "warning: {}", message_text(message));
}

/// `message` as it may be written to standard error: every control character
/// other than line feed and tab, and every text direction override, is
/// replaced with U+FFFD. A message can quote text from a vault (the database
/// does, when a value cannot be converted), and that text must not reach the
/// terminal as an escape sequence.
pub fn message_text(message: &str) -> String {
    message
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c,
            c if acts_on_terminal(c) => '\u{FFFD}',
            c => c,
        })
        .collect()
}

/// Run `write` on buffered standard output and flush.
fn to_stdout(
    write: impl FnOnce(&mut BufWriter<io::StdoutLock<'_>>) -> io::Result<()>,
) -> Result<(), Error> {
    let mut out = BufWriter::new(io::stdout().lock());
    let result = write(&mut out).and_then(|()| out.flush());
    result.map_err(stdout_error)
}

/// The command error for a failed write to standard output.
///
/// A reader that has seen enough closes the pipe (`b4 bookings list | head`).
/// Rust ignores SIGPIPE, so that arrives here as an error. It is not a
/// failure: the command stops without a message and with status 0.
fn stdout_error(error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::BrokenPipe {
        Error::exit(0)
    } else {
        Error::from_error(&error).context("cannot write to standard output")
    }
}

// --- Renderers ---------------------------------------------------------------

/// Write `table` to `out` in the format of `options`, as data: `csv`, `jsonl`
/// and `json` are byte-exact, which is right for a file or a pipe. See
/// [`render_table_on`] for a destination that may be a terminal.
pub fn render_table(out: &mut impl Write, table: &Table, options: &Options) -> io::Result<()> {
    render_table_on(out, table, options, false)
}

/// Write `table` to `out` in the format of `options`. `terminal` says whether
/// `out` is a terminal: then `csv`, `jsonl` and `json` are cleaned of the
/// characters a terminal acts on, as the module documentation describes.
pub fn render_table_on(
    out: &mut impl Write,
    table: &Table,
    options: &Options,
    terminal: bool,
) -> io::Result<()> {
    match options.format {
        Format::Table => render_text_table(out, table, options),
        Format::Json => {
            let mut rows = 0;
            write_json_rows(out, table, terminal, &mut rows)?;
            end_json_array(out, rows)
        }
        Format::Jsonl => {
            for row in &table.rows {
                writeln!(out, "{}", json_object(&table.columns, row, terminal))?;
            }
            Ok(())
        }
        Format::Csv => {
            if options.header {
                out.write_all(csv_header(&table.columns, terminal).as_bytes())?;
            }
            for row in &table.rows {
                out.write_all(csv_row(&table.columns, row, terminal).as_bytes())?;
            }
            Ok(())
        }
    }
}

/// Write the rows of `table` as elements of a JSON array that has `written`
/// elements so far: the opening bracket goes before the first element of
/// all, a comma between two.
fn write_json_rows(
    out: &mut impl Write,
    table: &Table,
    terminal: bool,
    written: &mut u64,
) -> io::Result<()> {
    for row in &table.rows {
        let lead = if *written == 0 { "[\n  " } else { ",\n  " };
        let object = json_object(&table.columns, row, terminal);
        write!(out, "{lead}{object}")?;
        *written += 1;
    }
    Ok(())
}

/// Close a JSON array of `written` elements; an empty one is `[]`.
fn end_json_array(out: &mut impl Write, written: u64) -> io::Result<()> {
    if written == 0 {
        out.write_all(b"[]\n")
    } else {
        out.write_all(b"\n]\n")
    }
}

// --- Results that arrive in chunks -------------------------------------------

/// Writes a result that arrives in chunks, such as the chunks of
/// `Database::for_each_chunk`, without holding it as a whole.
///
/// ```ignore
/// let mut stream = TableStream::stdout(globals);
/// database.for_each_chunk(sql, &[], &[], CHUNK_ROWS, |chunk| stream.write(chunk))?;
/// stream.finish()?;
/// ```
///
/// `csv`, `jsonl` and `json` are written chunk by chunk, so memory does not
/// grow with the result. If a later chunk fails, the rows before it have
/// been written already: the command then ends with an error and a partial
/// document, which for `json` is not valid JSON.
///
/// `table` needs every row before it can size the columns. Its rows are
/// buffered, up to [`MAX_TABLE_ROWS`] rows and [`MAX_TABLE_TEXT_BYTES`] of
/// text; a larger result is an error that says to use `--output csv` or
/// `--output jsonl`, or a `LIMIT`. Nothing has been written by then.
///
/// Every chunk must have the same columns. The output is the same, byte for
/// byte, as [`render_table_on`] of the whole result.
#[derive(Debug)]
pub struct TableStream<W: Write> {
    out: W,
    options: Options,
    terminal: bool,
    /// Whether the "no rows" note of an empty table is left out.
    quiet: bool,
    rows: u64,
    started: bool,
    /// The rows of the `table` format, which are written by `finish`.
    buffer: Option<Table>,
    buffered_text: usize,
}

impl TableStream<BufWriter<io::Stdout>> {
    /// A stream to standard output in the format the user asked for.
    pub fn stdout(globals: &Globals) -> TableStream<BufWriter<io::Stdout>> {
        let stdout = io::stdout();
        let terminal = stdout.is_terminal();
        let mut stream = TableStream::new(
            BufWriter::new(stdout),
            Options::for_destination(globals, terminal),
            terminal,
        );
        stream.quiet = globals.quiet;
        stream
    }
}

impl<W: Write> TableStream<W> {
    /// A stream to `out`. `terminal` says whether `out` is a terminal, as for
    /// [`render_table_on`].
    pub fn new(out: W, options: Options, terminal: bool) -> TableStream<W> {
        TableStream {
            out,
            options,
            terminal,
            // Only `stdout` prints the note; a stream elsewhere has no user
            // to tell.
            quiet: true,
            rows: 0,
            started: false,
            buffer: None,
            buffered_text: 0,
        }
    }

    /// Write the next chunk of the result, or keep it for `finish` in the
    /// `table` format. A reader that closed the pipe ends the command
    /// quietly, like [`print_table`].
    pub fn write(&mut self, chunk: Table) -> Result<(), Error> {
        if self.options.format == Format::Table {
            return self.keep(chunk);
        }
        let first = !self.started;
        self.started = true;
        let written = match self.options.format {
            Format::Csv => {
                let mut options = self.options;
                // The header belongs to the first chunk only.
                options.header = options.header && first;
                render_table_on(&mut self.out, &chunk, &options, self.terminal)
            }
            Format::Json => write_json_rows(&mut self.out, &chunk, self.terminal, &mut self.rows),
            Format::Jsonl => render_table_on(&mut self.out, &chunk, &self.options, self.terminal),
            // Kept above.
            Format::Table => Ok(()),
        };
        if self.options.format != Format::Json {
            self.rows += chunk.len() as u64;
        }
        written.map_err(stdout_error)
    }

    /// Add `chunk` to the rows of the `table` format.
    fn keep(&mut self, chunk: Table) -> Result<(), Error> {
        let text: usize = chunk
            .rows
            .iter()
            .flatten()
            .map(|cell| match cell {
                Cell::Text(text) => text.len(),
                _ => 0,
            })
            .sum();
        self.buffered_text = self.buffered_text.saturating_add(text);
        let buffer = self
            .buffer
            .get_or_insert_with(|| Table::new(chunk.columns.clone()));
        let too_many = buffer.rows.len().saturating_add(chunk.rows.len()) > MAX_TABLE_ROWS;
        if too_many || self.buffered_text > MAX_TABLE_TEXT_BYTES {
            let limit = if too_many {
                format!("{MAX_TABLE_ROWS} rows")
            } else {
                format!("{} MiB of text", MAX_TABLE_TEXT_BYTES / (1024 * 1024))
            };
            return Err(Error::new(format!(
                "the result has more than {limit}, which is all the table format holds in \
                 memory: use --output csv or --output jsonl, or add a LIMIT to the query"
            )));
        }
        buffer.rows.extend(chunk.rows);
        self.rows = buffer.rows.len() as u64;
        Ok(())
    }

    /// Write what is still to be written (the buffered `table`, the end of a
    /// `json` array) and flush. Returns the number of rows of the result.
    ///
    /// An empty result in the `table` format prints nothing and, on a stream
    /// to standard output without `--quiet`, the note `no rows` to standard
    /// error.
    pub fn finish(mut self) -> Result<u64, Error> {
        let written = match self.options.format {
            Format::Table => match self.buffer.take() {
                Some(table) => render_text_table(&mut self.out, &table, &self.options),
                None => Ok(()),
            },
            Format::Json => end_json_array(&mut self.out, self.rows),
            Format::Jsonl | Format::Csv => Ok(()),
        };
        written
            .and_then(|()| self.out.flush())
            .map_err(stdout_error)?;
        if self.rows == 0 && self.options.format == Format::Table && !self.quiet {
            let _ = writeln!(io::stderr().lock(), "no rows");
        }
        Ok(self.rows)
    }
}

/// Write `record` to `out` in the format of `options`. `terminal` says
/// whether `out` is a terminal, as for [`render_table_on`]: if it is not, the
/// machine formats are byte-exact.
///
/// * `table`: one line per field, `Title:  value`, values aligned. A value of
///   several lines continues below itself. Nothing is cut.
/// * `json`: one object, one key per line.
/// * `jsonl`: the same object on one line.
/// * `csv`: a header line and one row.
pub fn render_record_on(
    out: &mut impl Write,
    record: &Record,
    options: &Options,
    terminal: bool,
) -> io::Result<()> {
    match options.format {
        Format::Table => render_text_record(out, record),
        Format::Json => {
            if record.columns.is_empty() {
                return out.write_all(b"{}\n");
            }
            out.write_all(b"{\n")?;
            let last = record.columns.len().saturating_sub(1);
            for (index, (column, cell)) in record.fields().enumerate() {
                let separator = if index == last { "" } else { "," };
                let key = json_string(&column.name, terminal);
                let value = json_value(column, cell, terminal);
                writeln!(out, "  {key}: {value}{separator}")?;
            }
            out.write_all(b"}\n")
        }
        Format::Jsonl => writeln!(
            out,
            "{}",
            json_object(&record.columns, &record.cells, terminal)
        ),
        Format::Csv => {
            if options.header {
                out.write_all(csv_header(&record.columns, terminal).as_bytes())?;
            }
            out.write_all(csv_row(&record.columns, &record.cells, terminal).as_bytes())
        }
    }
}

/// The cell of `row` under the column at `index`; missing cells are null.
fn cell_at(row: &[Cell], index: usize) -> &Cell {
    row.get(index).unwrap_or(&Cell::Null)
}

// --- Table format ------------------------------------------------------------

/// One column of a text table, formatted and measured.
struct TextColumn {
    title: String,
    cells: Vec<String>,
    width: usize,
    right_aligned: bool,
    flexible: bool,
}

fn render_text_table(out: &mut impl Write, table: &Table, options: &Options) -> io::Result<()> {
    if table.is_empty() {
        return Ok(());
    }

    let mut columns: Vec<TextColumn> = Vec::new();
    for (index, column) in table.columns.iter().enumerate() {
        // A currency that a money column already shows is not repeated.
        if is_shown_by_money_column(&table.columns, column) {
            continue;
        }
        let cells = match &column.kind {
            Kind::Money { currency } => {
                let currency_index = table.columns.iter().position(|c| &c.name == currency);
                money_cells(&table.rows, index, currency_index)
            }
            _ => table
                .rows
                .iter()
                .map(|row| table_text(column, cell_at(row, index)))
                .collect(),
        };
        // A title is not always b4's own: `b4 sql` takes it from the query.
        let title = if options.header {
            terminal_text(&column.title.to_uppercase(), " ")
        } else {
            String::new()
        };
        let width = cells
            .iter()
            .map(|c| display_width(c))
            .fold(display_width(&title), usize::max);
        columns.push(TextColumn {
            title,
            cells,
            width,
            right_aligned: column.kind.is_right_aligned(),
            flexible: column.flexible && column.kind == Kind::Text,
        });
    }

    if let Some(width) = options.width {
        fit_to_width(&mut columns, width);
    }

    let mut line = String::new();
    if options.header {
        write_line(out, &mut line, &columns, |column| &column.title)?;
    }
    for row in 0..table.rows.len() {
        write_line(out, &mut line, &columns, |column| {
            column.cells.get(row).map_or("", String::as_str)
        })?;
    }
    Ok(())
}

/// Whether `column` holds the currency of a money column of the same table.
fn is_shown_by_money_column(columns: &[Column], column: &Column) -> bool {
    columns
        .iter()
        .any(|c| matches!(&c.kind, Kind::Money { currency } if currency == &column.name))
}

/// Write one table line: the text `text_of` returns for every column, padded
/// or cut to the column's width, without trailing blanks.
fn write_line<'a>(
    out: &mut impl Write,
    line: &mut String,
    columns: &'a [TextColumn],
    text_of: impl Fn(&'a TextColumn) -> &'a str,
) -> io::Result<()> {
    line.clear();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            push_blanks(line, GUTTER);
        }
        let text = text_of(column);
        let cut;
        let (text, text_width) = match display_width(text) {
            width if width > column.width => {
                cut = truncate(text, column.width);
                (cut.as_str(), display_width(&cut))
            }
            width => (text, width),
        };
        let padding = column.width.saturating_sub(text_width);
        if column.right_aligned {
            push_blanks(line, padding);
            line.push_str(text);
        } else {
            line.push_str(text);
            push_blanks(line, padding);
        }
    }
    let trimmed = line.trim_end_matches(' ');
    out.write_all(trimmed.as_bytes())?;
    out.write_all(b"\n")
}

fn push_blanks(line: &mut String, count: usize) {
    line.extend(std::iter::repeat_n(' ', count));
}

/// The cells of a money column: amounts right-aligned among themselves, each
/// followed by its currency.
fn money_cells(rows: &[Vec<Cell>], index: usize, currency_index: Option<usize>) -> Vec<String> {
    let parts: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            let amount = match cell_at(row, index) {
                Cell::Decimal(value) => decimal_text(*value, MONEY_SCALE),
                // Null, or a value of another type in a money column.
                other => return (plain_text(other), String::new()),
            };
            let currency = match currency_index.map(|i| cell_at(row, i)) {
                Some(Cell::Text(code)) => terminal_text(code, " "),
                _ => String::new(),
            };
            (amount, currency)
        })
        .collect();

    // Amounts line up on their right edge and currencies on their left, also
    // in a row that has no currency.
    let amount_width = parts
        .iter()
        .map(|(amount, _)| display_width(amount))
        .max()
        .unwrap_or(0);
    let currency_width = parts
        .iter()
        .map(|(_, currency)| display_width(currency))
        .max()
        .unwrap_or(0);
    parts
        .into_iter()
        .map(|(amount, currency)| {
            let mut cell = String::new();
            push_blanks(
                &mut cell,
                amount_width.saturating_sub(display_width(&amount)),
            );
            cell.push_str(&amount);
            if currency_width > 0 {
                cell.push(' ');
                cell.push_str(&currency);
                push_blanks(
                    &mut cell,
                    currency_width.saturating_sub(display_width(&currency)),
                );
            }
            cell
        })
        .collect()
}

/// Narrow the flexible columns until the table fits into `width` terminal
/// columns, widest first. If it cannot fit, the flexible columns end at their
/// minimum width and the terminal wraps the rest.
fn fit_to_width(columns: &mut [TextColumn], width: usize) {
    let gutters = columns.len().saturating_sub(1).saturating_mul(GUTTER);
    let total = columns
        .iter()
        .fold(gutters, |sum, c| sum.saturating_add(c.width));
    if total <= width {
        return;
    }
    let fixed = columns
        .iter()
        .filter(|c| !c.flexible)
        .fold(gutters, |sum, c| sum.saturating_add(c.width));
    let budget = width.saturating_sub(fixed);

    // A column keeps at least its title, or a few characters of content.
    let floor = |column: &TextColumn| {
        column
            .width
            .min(display_width(&column.title).max(MIN_FLEXIBLE_WIDTH))
    };
    let used = |cap: usize| {
        columns
            .iter()
            .filter(|c| c.flexible)
            .fold(0usize, |sum, c| {
                sum.saturating_add(c.width.min(cap).max(floor(c)))
            })
    };

    // The largest common cap that fits: cutting every column down to one cap
    // takes from the widest column first and never from a narrow one.
    let mut low = 0;
    let mut high = columns
        .iter()
        .filter(|c| c.flexible)
        .map(|c| c.width)
        .max()
        .unwrap_or(0);
    while low < high {
        let middle = low + (high - low).div_ceil(2);
        if used(middle) <= budget {
            low = middle;
        } else {
            high = middle - 1;
        }
    }
    let cap = low;
    for column in columns.iter_mut().filter(|c| c.flexible) {
        column.width = column.width.min(cap).max(floor(column));
    }
}

fn render_text_record(out: &mut impl Write, record: &Record) -> io::Result<()> {
    let titles: Vec<String> = record
        .columns
        .iter()
        .map(|column| terminal_text(&column.title, " "))
        .collect();
    // The label is the title and a colon.
    let label_width = titles
        .iter()
        .map(|title| display_width(title).saturating_add(1))
        .max()
        .unwrap_or(0);

    let mut line = String::new();
    for (index, (column, cell)) in record.fields().enumerate() {
        let value = match (&column.kind, cell) {
            (Kind::Money { currency }, Cell::Decimal(amount)) => {
                let mut text = decimal_text(*amount, MONEY_SCALE);
                let code = record.fields().find(|(c, _)| &c.name == currency);
                if let Some((_, Cell::Text(code))) = code {
                    text.push(' ');
                    text.push_str(code);
                }
                text
            }
            (_, Cell::Text(text)) => text.clone(),
            _ => table_text(column, cell_at(&record.cells, index)),
        };

        // A value of several lines continues under its first line.
        let mut lines = value.lines();
        line.clear();
        let title = titles.get(index).map_or("", String::as_str);
        line.push_str(title);
        line.push(':');
        push_blanks(
            &mut line,
            label_width.saturating_sub(display_width(title) + 1),
        );
        push_blanks(&mut line, GUTTER);
        line.push_str(&terminal_text(lines.next().unwrap_or(""), " "));
        writeln!(out, "{}", line.trim_end_matches(' '))?;
        for continuation in lines {
            line.clear();
            push_blanks(&mut line, label_width.saturating_add(GUTTER));
            line.push_str(&terminal_text(continuation, " "));
            writeln!(out, "{}", line.trim_end_matches(' '))?;
        }
    }
    Ok(())
}

/// The text of a cell in a table.
fn table_text(column: &Column, cell: &Cell) -> String {
    match (cell, &column.kind) {
        (Cell::Text(text), _) => terminal_text(text, LINE_BREAK),
        (Cell::Decimal(value), Kind::Decimal { scale }) => decimal_fixed(*value, *scale),
        (Cell::Decimal(value), Kind::Money { .. }) => decimal_text(*value, MONEY_SCALE),
        (Cell::Timestamp(value), _) => value.text(' '),
        _ => plain_text(cell),
    }
}

/// The text of a cell without regard to its column: what CSV writes, and what
/// a table writes where the column adds nothing.
fn plain_text(cell: &Cell) -> String {
    match cell {
        Cell::Null => String::new(),
        Cell::Text(text) => text.clone(),
        Cell::Integer(value) => value.to_string(),
        Cell::Float(value) => float_text(*value),
        Cell::Decimal(value) => decimal_text(*value, 0),
        Cell::Date(value) => value.to_string(),
        Cell::Timestamp(value) => value.to_string(),
        Cell::Boolean(true) => "true".to_owned(),
        Cell::Boolean(false) => "false".to_owned(),
    }
}

fn float_text(value: f64) -> String {
    if value.is_nan() {
        "NaN".to_owned()
    } else if value.is_infinite() {
        if value > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else {
        // Rust prints the shortest text that reads back as the same double,
        // and never uses an exponent.
        value.to_string()
    }
}

/// `text` made safe and single-line for a terminal: every line break becomes
/// `line_break`, a tab becomes a blank, and any other control character or
/// text direction override becomes U+FFFD.
fn terminal_text(text: &str, line_break: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                safe.push_str(line_break);
            }
            '\n' => safe.push_str(line_break),
            '\t' => safe.push(' '),
            c if acts_on_terminal(c) => safe.push('\u{FFFD}'),
            c => safe.push(c),
        }
    }
    safe
}

/// Whether a terminal acts on `c` instead of showing it: a control character
/// (C0 with ESC, DEL, and C1, where U+009B starts an escape sequence just as
/// ESC `[` does), or an override or isolate of the bidirectional algorithm
/// (U+202A to U+202E, U+2066 to U+2069), which can make a line read
/// differently from what it holds.
pub(crate) fn acts_on_terminal(c: char) -> bool {
    c.is_control() || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// A CSV field for a terminal: like [`terminal_text`], but the tab and the
/// line breaks LF and CRLF stay, because CSV quoting keeps them inside the
/// field. A carriage return on its own is replaced: it would let the text
/// overwrite the start of its line.
fn terminal_csv_text(text: &str) -> String {
    let mut safe = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\n' | '\t' => safe.push(c),
            '\r' if chars.peek() == Some(&'\n') => safe.push(c),
            c if acts_on_terminal(c) => safe.push('\u{FFFD}'),
            c => safe.push(c),
        }
    }
    safe
}

/// `json`, any JSON text, for a terminal: every character a terminal acts on
/// becomes a `\uXXXX` escape. Such a character can only stand inside a
/// string (outside of strings JSON is ASCII, and the line feeds and blanks of
/// the layout are kept), so the result is the same document.
pub fn json_for_terminal(json: &str) -> String {
    let mut safe = String::with_capacity(json.len());
    for c in json.chars() {
        match c {
            '\n' => safe.push(c),
            c if acts_on_terminal(c) => push_json_escape(&mut safe, c),
            c => safe.push(c),
        }
    }
    safe
}

/// Append `c` as `\uXXXX` (two of them for a character beyond U+FFFF).
fn push_json_escape(literal: &mut String, c: char) {
    let mut units = [0u16; 2];
    for unit in c.encode_utf16(&mut units) {
        literal.push_str(&format!("\\u{unit:04x}"));
    }
}

// --- Decimals ----------------------------------------------------------------

/// The exact decimal text of `value`: no trailing zeros beyond `min_scale`
/// decimals, no exponent, no negative zero.
fn decimal_text(value: Decimal, min_scale: u32) -> String {
    let value = value.normalize();
    let mantissa = value.mantissa();
    let scale = value.scale() as usize;
    let digits = mantissa.unsigned_abs().to_string();

    let mut text = String::with_capacity(digits.len() + 3);
    if mantissa < 0 {
        text.push('-');
    }
    // Split the digits at the decimal point; a short mantissa gets zeros in
    // front ("5" at scale 3 is 0.005).
    let (whole, fraction) = match digits.len().checked_sub(scale) {
        Some(split) if split > 0 => (digits.get(..split), digits.get(split..)),
        _ => (None, None),
    };
    match (whole, fraction) {
        (Some(whole), Some(fraction)) => {
            text.push_str(whole);
            if !fraction.is_empty() || min_scale > 0 {
                text.push('.');
                text.push_str(fraction);
            }
        }
        _ => {
            text.push_str("0.");
            text.extend(std::iter::repeat_n('0', scale - digits.len()));
            text.push_str(&digits);
        }
    }
    let missing = (min_scale as usize).saturating_sub(scale);
    text.extend(std::iter::repeat_n('0', missing));
    text
}

/// `value` rounded half away from zero to `scale` decimals, with exactly that
/// many decimals.
fn decimal_fixed(value: Decimal, scale: u32) -> String {
    let scale = scale.min(MAX_SCALE);
    let rounded = value.round_dp_with_strategy(scale, RoundingStrategy::MidpointAwayFromZero);
    decimal_text(rounded, scale)
}

/// The number of significant decimal digits of `value`: its digits without
/// leading and trailing zeros.
fn significant_digits(value: Decimal) -> usize {
    let digits = value.mantissa().unsigned_abs().to_string();
    digits.trim_end_matches('0').len().max(1)
}

// --- JSON --------------------------------------------------------------------

/// One row as a JSON object on one line, keys in column order. `terminal`
/// as for [`json_string`].
fn json_object(columns: &[Column], row: &[Cell], terminal: bool) -> String {
    let mut object = String::from("{");
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            object.push(',');
        }
        object.push_str(&json_string(&column.name, terminal));
        object.push(':');
        object.push_str(&json_value(column, cell_at(row, index), terminal));
    }
    object.push('}');
    object
}

fn json_value(column: &Column, cell: &Cell, terminal: bool) -> String {
    match cell {
        Cell::Null => "null".to_owned(),
        Cell::Text(text) => json_string(text, terminal),
        // Beyond 2^53 a double no longer holds every integer: a JSON parser
        // would round the number silently, so it travels as text.
        Cell::Integer(value) if value.unsigned_abs() > MAX_SAFE_INTEGER => {
            format!("\"{value}\"")
        }
        Cell::Integer(_) | Cell::Boolean(_) => plain_text(cell),
        Cell::Float(value) if value.is_finite() => float_text(*value),
        Cell::Float(value) => json_string(&float_text(*value), terminal),
        Cell::Decimal(value) => {
            let text = machine_decimal(column, *value);
            // More digits than a double holds: the same, as text.
            if significant_digits(*value) <= DOUBLE_DIGITS {
                text
            } else {
                format!("\"{text}\"")
            }
        }
        Cell::Date(_) | Cell::Timestamp(_) => format!("\"{}\"", plain_text(cell)),
    }
}

/// The exact text of a decimal in JSON and CSV.
fn machine_decimal(column: &Column, value: Decimal) -> String {
    let min_scale = if matches!(column.kind, Kind::Money { .. }) {
        MONEY_SCALE
    } else {
        0
    };
    decimal_text(value, min_scale)
}

/// `text` as a JSON string literal. The characters below U+0020 are always
/// escaped, as JSON demands. For a `terminal` the other characters a
/// terminal acts on (DEL, the C1 controls, the direction overrides) are
/// escaped as well, which a parser reads back as the same text.
fn json_string(text: &str, terminal: bool) -> String {
    let mut literal = String::with_capacity(text.len() + 2);
    literal.push('"');
    for c in text.chars() {
        match c {
            '"' => literal.push_str("\\\""),
            '\\' => literal.push_str("\\\\"),
            '\n' => literal.push_str("\\n"),
            '\r' => literal.push_str("\\r"),
            '\t' => literal.push_str("\\t"),
            c if u32::from(c) < 0x20 => push_json_escape(&mut literal, c),
            c if terminal && acts_on_terminal(c) => push_json_escape(&mut literal, c),
            c => literal.push(c),
        }
    }
    literal.push('"');
    literal
}

// --- CSV ---------------------------------------------------------------------

/// The header line. `terminal` as for [`csv_row`].
fn csv_header(columns: &[Column], terminal: bool) -> String {
    let mut line = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        if terminal {
            push_csv_field(&mut line, &terminal_csv_text(&column.name));
        } else {
            push_csv_field(&mut line, &column.name);
        }
    }
    line.push('\n');
    line
}

/// One row. For a `terminal` the text is cleaned with [`terminal_csv_text`];
/// otherwise the fields are the exact values.
fn csv_row(columns: &[Column], row: &[Cell], terminal: bool) -> String {
    let mut line = String::new();
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            line.push(',');
        }
        match cell_at(row, index) {
            Cell::Null => {}
            // Quoted, to tell an empty text from a missing value.
            Cell::Text(text) if text.is_empty() => line.push_str("\"\""),
            Cell::Decimal(value) => line.push_str(&machine_decimal(column, *value)),
            Cell::Text(text) if terminal => push_csv_field(&mut line, &terminal_csv_text(text)),
            cell => push_csv_field(&mut line, &plain_text(cell)),
        }
    }
    line.push('\n');
    line
}

/// Append a field, quoted when RFC 4180 requires it. Blanks at either end are
/// quoted as well, because some readers trim unquoted fields.
fn push_csv_field(line: &mut String, field: &str) {
    let needs_quotes =
        field.contains([',', '"', '\n', '\r']) || field.starts_with(' ') || field.ends_with(' ');
    if !needs_quotes {
        line.push_str(field);
        return;
    }
    line.push('"');
    for c in field.chars() {
        if c == '"' {
            line.push('"');
        }
        line.push(c);
    }
    line.push('"');
}

// --- Display width -----------------------------------------------------------

/// The number of terminal columns `text` occupies, approximately.
///
/// This is not the Unicode algorithm, which needs tables b4 does not carry.
/// It counts one column per character, none for combining marks, zero-width
/// characters and variation selectors, and two for the East Asian wide and
/// fullwidth blocks and for emoji. That is exact for the Latin, Greek and
/// Cyrillic text found in bank statements, with or without combining accents.
/// It can be off by a column for emoji sequences joined with U+200D and for
/// scripts with complex shaping; then a table row is slightly out of line,
/// nothing worse.
pub fn display_width(text: &str) -> usize {
    text.chars().map(char_width).sum()
}

fn char_width(c: char) -> usize {
    match u32::from(c) {
        // Control characters. Table text has none left by the time it is
        // measured.
        0x00..=0x1F | 0x7F..=0x9F => 0,
        // Combining marks.
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x1AB0..=0x1AFF
        | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF
        | 0xFE20..=0xFE2F => 0,
        // Zero-width spaces and joiners, direction marks, word joiner, BOM.
        0x200B..=0x200F | 0x2060..=0x2064 | 0xFEFF => 0,
        // Variation selectors.
        0xFE00..=0xFE0F | 0xE0100..=0xE01EF => 0,
        // Hangul Jamo, CJK, Kana, Yi, Hangul syllables, compatibility and
        // fullwidth forms.
        0x1100..=0x115F
        | 0x2E80..=0x303E
        | 0x3041..=0x33FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xA000..=0xA4CF
        | 0xA960..=0xA97F
        | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF
        | 0xFE30..=0xFE6F
        | 0xFF00..=0xFF60
        | 0xFFE0..=0xFFE6 => 2,
        // Emoji and pictographs.
        0x1F300..=0x1F64F | 0x1F680..=0x1F6FF | 0x1F900..=0x1F9FF | 0x1FA70..=0x1FAFF => 2,
        // CJK extensions.
        0x20000..=0x3FFFD => 2,
        _ => 1,
    }
}

/// `text` cut to at most `width` terminal columns, the cut marked with `…`.
fn truncate(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_owned();
    }
    let Some(budget) = width.checked_sub(1) else {
        return String::new();
    };
    let mut cut = String::new();
    let mut used = 0;
    for c in text.chars() {
        let char_width = char_width(c);
        if used + char_width > budget {
            break;
        }
        used += char_width;
        cut.push(c);
    }
    // Blanks before the mark would look like a gap between columns.
    cut.truncate(cut.trim_end_matches(' ').len());
    cut.push(ELLIPSIS);
    cut
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;

    fn dec(text: &str) -> Decimal {
        Decimal::from_str(text).unwrap()
    }

    fn options(format: Format) -> Options {
        Options {
            format,
            header: true,
            width: None,
        }
    }

    fn rendered(table: &Table, options: &Options) -> String {
        let mut out = Vec::new();
        render_table(&mut out, table, options).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn rendered_record(record: &Record, options: &Options) -> String {
        let mut out = Vec::new();
        render_record_on(&mut out, record, options, false).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Three bookings with every column kind the commands use.
    fn bookings() -> Table {
        let mut table = Table::new(vec![
            Column::integer("id", "Id"),
            Column::new("booking_date", "Date", Kind::Date),
            Column::money("amount", "Amount", "currency"),
            Column::text("currency", "Currency"),
            Column::text("payee", "Payee"),
            Column::text("purpose", "Purpose").flexible(),
            Column::boolean("pending", "Pending"),
        ]);
        table.push(vec![
            Cell::Integer(7),
            Date::new(2026, 1, 5).into(),
            dec("-12.5").into(),
            "EUR".into(),
            "Bakery".into(),
            "Bread and rolls".into(),
            false.into(),
        ]);
        table.push(vec![
            Cell::Integer(1234),
            Date::new(2026, 1, 31).into(),
            dec("2500.00").into(),
            "EUR".into(),
            "Employer Ltd".into(),
            "Salary January\nThank you".into(),
            false.into(),
        ]);
        table.push(vec![
            Cell::Integer(8),
            Cell::Null,
            dec("-0.999").into(),
            Cell::Null,
            Cell::Null,
            Cell::Null,
            true.into(),
        ]);
        table
    }

    // --- dates ---

    #[test]
    fn date_rejects_days_that_do_not_exist() {
        assert!(Date::new(2024, 2, 29).is_some());
        assert!(Date::new(2023, 2, 29).is_none());
        assert!(Date::new(1900, 2, 29).is_none());
        assert!(Date::new(2000, 2, 29).is_some());
        assert!(Date::new(2026, 4, 31).is_none());
        assert!(Date::new(2026, 0, 1).is_none());
        assert!(Date::new(2026, 13, 1).is_none());
        assert!(Date::new(2026, 1, 0).is_none());
    }

    #[test]
    fn date_converts_from_and_to_days_since_epoch() {
        assert_eq!(Date::from_days_since_epoch(0).to_string(), "1970-01-01");
        assert_eq!(Date::from_days_since_epoch(-1).to_string(), "1969-12-31");
        assert_eq!(
            Date::from_days_since_epoch(11_016).to_string(),
            "2000-02-29"
        );
        assert_eq!(
            Date::from_days_since_epoch(20_454).to_string(),
            "2026-01-01"
        );
        assert_eq!(
            Date::from_days_since_epoch(-719_162).to_string(),
            "0001-01-01"
        );

        // Every day of eight centuries survives the round trip and follows
        // the day before it.
        let mut previous = Date::from_days_since_epoch(-146_098);
        for days in -146_097..=146_097 {
            let date = Date::from_days_since_epoch(days);
            assert_eq!(date.days_since_epoch(), i64::from(days));
            assert!(Date::new(date.year(), date.month(), date.day()).is_some());
            assert!(date > previous);
            previous = date;
        }
    }

    #[test]
    fn date_conversion_is_total() {
        // The extremes must not overflow or panic.
        let _ = Date::from_days_since_epoch(i32::MAX).to_string();
        let _ = Date::from_days_since_epoch(i32::MIN).to_string();
        let _ = Timestamp::from_micros(i64::MAX).to_string();
        let _ = Timestamp::from_micros(i64::MIN).to_string();
    }

    #[test]
    fn timestamp_prints_iso_and_fraction_only_when_present() {
        let noon = Timestamp::from_micros(20_454 * MICROS_PER_DAY + 12 * 3_600 * 1_000_000);
        assert_eq!(noon.to_string(), "2026-01-01T12:00:00");
        assert_eq!(noon.text(' '), "2026-01-01 12:00:00");
        assert_eq!(
            Timestamp::from_micros(1_500_000).to_string(),
            "1970-01-01T00:00:01.500000"
        );
        // Before the epoch the time of day still counts forward.
        assert_eq!(
            Timestamp::from_micros(-1_000_000).to_string(),
            "1969-12-31T23:59:59"
        );
        assert_eq!(noon.date(), Date::new(2026, 1, 1).unwrap());
    }

    // --- decimals ---

    #[test]
    fn decimal_text_is_exact_and_minimal() {
        assert_eq!(decimal_text(dec("12.3400"), 0), "12.34");
        assert_eq!(decimal_text(dec("12.3400"), 2), "12.34");
        assert_eq!(decimal_text(dec("12.3"), 2), "12.30");
        assert_eq!(decimal_text(dec("12"), 2), "12.00");
        assert_eq!(decimal_text(dec("12.00"), 0), "12");
        assert_eq!(decimal_text(dec("-0.005"), 2), "-0.005");
        assert_eq!(decimal_text(dec("0.5"), 0), "0.5");
        assert_eq!(decimal_text(dec("-1234567.891"), 2), "-1234567.891");
        assert_eq!(decimal_text(dec("0"), 0), "0");
        assert_eq!(decimal_text(dec("0.000"), 2), "0.00");
        assert_eq!(decimal_text(dec("100"), 0), "100");
        // The extremes of a 96-bit decimal.
        assert_eq!(
            decimal_text(Decimal::MAX, 0),
            "79228162514264337593543950335"
        );
        assert_eq!(
            decimal_text(Decimal::MIN, 2),
            "-79228162514264337593543950335.00"
        );
        assert_eq!(
            decimal_text(dec("0.0000000000000000000000000001"), 0),
            "0.0000000000000000000000000001"
        );
    }

    #[test]
    fn negative_zero_prints_without_a_sign() {
        let mut zero = dec("0.00");
        zero.set_sign_negative(true);
        assert_eq!(decimal_text(zero, 2), "0.00");
        assert_eq!(decimal_fixed(dec("-0.001"), 2), "0.00");
    }

    #[test]
    fn decimal_fixed_rounds_half_away_from_zero() {
        assert_eq!(decimal_fixed(dec("1.005"), 2), "1.01");
        assert_eq!(decimal_fixed(dec("-1.005"), 2), "-1.01");
        assert_eq!(decimal_fixed(dec("1.0049"), 2), "1.00");
        assert_eq!(decimal_fixed(dec("3"), 4), "3.0000");
        assert_eq!(decimal_fixed(dec("2.5"), 0), "3");
        // A scale beyond what a decimal can hold is clamped, not an error.
        assert_eq!(decimal_fixed(dec("1"), 99).len(), "1.".len() + 28);
    }

    #[test]
    fn json_writes_a_decimal_as_a_number_only_when_a_double_holds_it() {
        let column = Column::decimal("value", "Value", 2);
        let json = |text: &str| json_value(&column, &Cell::Decimal(dec(text)), false);

        assert_eq!(json("12.34"), "12.34");
        assert_eq!(json("-0.10"), "-0.1");
        assert_eq!(json("0"), "0");
        // 15 significant digits is the limit.
        assert_eq!(json("1234567890123.45"), "1234567890123.45");
        assert_eq!(json("12345678901234.56"), "\"12345678901234.56\"");
        assert_eq!(json("0.1234567890123456"), "\"0.1234567890123456\"");
        // Trailing zeros are not significant.
        assert_eq!(json("123456789012345000"), "123456789012345000");
        assert_eq!(json("1.230000000000000000"), "1.23");
    }

    #[test]
    fn json_numbers_survive_a_double_round_trip() {
        // The claim behind the 15-digit rule: what is written as a number
        // reads back, through a double, as the same decimal.
        let column = Column::decimal("value", "Value", 2);
        for text in [
            "0.1",
            "0.3",
            "-99999999999999.9",
            "999999999999999",
            "0.000000000000001",
            "123456.789012345",
            "0.0000000000000000000000000001",
            "79228162514264300000000000000",
        ] {
            let value = dec(text);
            let json = json_value(&column, &Cell::Decimal(value), false);
            assert!(!json.starts_with('"'), "{text} should be a number");
            let double: f64 = json.parse().unwrap();
            let back = Decimal::from_scientific(&format!("{double:e}")).unwrap();
            assert_eq!(back, value, "{text} changed in a double");
        }
    }

    // --- table ---

    #[test]
    fn table_aligns_columns_and_merges_the_currency() {
        let text = rendered(&bookings(), &options(Format::Table));
        // Numbers and their titles are right-aligned. The amounts line up on
        // the right edge of the number, also where the currency is missing.
        let expected = [
            "  ID  DATE             AMOUNT  PAYEE         PURPOSE                     PENDING",
            "   7  2026-01-05   -12.50 EUR  Bakery        Bread and rolls             false",
            "1234  2026-01-31  2500.00 EUR  Employer Ltd  Salary January ↵ Thank you  false",
            "   8               -0.999                                                true",
        ]
        .join("\n")
            + "\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn table_lines_have_no_trailing_blanks() {
        let text = rendered(&bookings(), &options(Format::Table));
        assert!(text.lines().all(|line| !line.ends_with(' ')));
    }

    #[test]
    fn table_without_header() {
        let mut table = Table::new(vec![Column::integer("id", "Identifier")]);
        table.push(vec![Cell::Integer(1)]);
        let text = rendered(
            &table,
            &Options {
                header: false,
                ..options(Format::Table)
            },
        );
        // The title no longer widens the column.
        assert_eq!(text, "1\n");
    }

    #[test]
    fn table_cuts_the_flexible_column_to_the_terminal_width() {
        let table = bookings();
        let full = rendered(&table, &options(Format::Table));
        let full_width = full.lines().map(display_width).max().unwrap();

        // Wide enough: unchanged.
        let wide = Options {
            width: Some(full_width),
            ..options(Format::Table)
        };
        assert_eq!(rendered(&table, &wide), full);

        let narrow = Options {
            width: Some(70),
            ..options(Format::Table)
        };
        let text = rendered(&table, &narrow);
        // Only the flexible column gave way, and the columns after it still
        // line up.
        let expected = [
            "  ID  DATE             AMOUNT  PAYEE         PURPOSE           PENDING",
            "   7  2026-01-05   -12.50 EUR  Bakery        Bread and rolls   false",
            "1234  2026-01-31  2500.00 EUR  Employer Ltd  Salary January…   false",
            "   8               -0.999                                      true",
        ]
        .join("\n")
            + "\n";
        assert_eq!(text, expected);
        assert!(text.lines().all(|line| display_width(line) <= 70));

        // One column less and the next cell is cut as well.
        let narrower = Options {
            width: Some(62),
            ..options(Format::Table)
        };
        let text = rendered(&table, &narrower);
        let expected = [
            "  ID  DATE             AMOUNT  PAYEE         PURPOSE   PENDING",
            "   7  2026-01-05   -12.50 EUR  Bakery        Bread a…  false",
            "1234  2026-01-31  2500.00 EUR  Employer Ltd  Salary…   false",
            "   8               -0.999                              true",
        ]
        .join("\n")
            + "\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn table_does_not_cut_without_a_terminal() {
        let mut table = Table::new(vec![Column::text("purpose", "Purpose").flexible()]);
        let long = "x".repeat(500);
        table.push(vec![long.as_str().into()]);
        let text = rendered(&table, &options(Format::Table));
        assert!(text.contains(&long));
    }

    #[test]
    fn table_never_cuts_a_column_that_is_not_flexible() {
        let mut table = Table::new(vec![
            Column::text("iban", "IBAN"),
            Column::text("purpose", "Purpose").flexible(),
        ]);
        table.push(vec![
            "DE00 0000 0000 0000 0000 00".into(),
            "Some long purpose text".into(),
        ]);
        let text = rendered(
            &table,
            &Options {
                width: Some(20),
                ..options(Format::Table)
            },
        );
        // Too narrow even at the minimum: the fixed column stays whole, the
        // flexible one stops at its floor and the terminal wraps the line.
        assert!(text.contains("DE00 0000 0000 0000 0000 00"));
        assert!(text.contains("Some lo…"));
    }

    #[test]
    fn table_cuts_the_widest_flexible_column_first() {
        let mut table = Table::new(vec![
            Column::text("notes", "Notes").flexible(),
            Column::text("purpose", "Purpose").flexible(),
        ]);
        table.push(vec![
            "short note".into(),
            "a purpose that goes on and on and on".into(),
        ]);
        let text = rendered(
            &table,
            &Options {
                width: Some(34),
                ..options(Format::Table)
            },
        );
        assert!(text.contains("short note"), "{text}");
        assert!(text.contains("a purpose that goes o…"), "{text}");
        assert!(text.lines().all(|line| display_width(line) <= 34));
    }

    #[test]
    fn table_shares_the_width_between_equally_wide_columns() {
        let mut table = Table::new(vec![
            Column::text("a", "A").flexible(),
            Column::text("b", "B").flexible(),
        ]);
        table.push(vec![
            "a".repeat(40).as_str().into(),
            "b".repeat(40).as_str().into(),
        ]);
        let text = rendered(
            &table,
            &Options {
                width: Some(42),
                ..options(Format::Table)
            },
        );
        let row = text.lines().nth(1).unwrap();
        assert_eq!(row, format!("{}…  {}…", "a".repeat(19), "b".repeat(19)));
    }

    #[test]
    fn table_replaces_control_characters() {
        let mut table = Table::new(vec![Column::text("purpose", "Purpose")]);
        table.push(vec!["a\x1b[31mred\x07\tb\r\nc\u{202E}d".into()]);
        let text = rendered(&table, &options(Format::Table));
        assert!(!text.contains('\x1b'));
        assert!(!text.contains('\x07'));
        assert!(!text.contains('\u{202E}'));
        assert_eq!(
            text.lines().nth(1).unwrap(),
            "a\u{FFFD}[31mred\u{FFFD} b ↵ c\u{FFFD}d"
        );
        // The header and one row: the line break did not start a new line.
        assert_eq!(text.lines().count(), 2);
    }

    #[test]
    fn titles_are_made_safe_like_cells() {
        let mut table = Table::new(vec![Column::text("x", "a\x1b[2Jb\nc")]);
        table.push(vec!["1".into()]);
        assert_eq!(
            rendered(&table, &options(Format::Table)),
            "A\u{FFFD}[2JB C\n1\n"
        );

        let mut record = Record::new();
        record.push(Column::text("x", "a\x1b[2Jb"), "1");
        assert_eq!(
            rendered_record(&record, &options(Format::Table)),
            "a\u{FFFD}[2Jb:  1\n"
        );
    }

    #[test]
    fn table_measures_wide_and_combining_characters() {
        assert_eq!(display_width("Zürich"), 6);
        // u followed by a combining diaeresis is one column.
        assert_eq!(display_width("Zu\u{308}rich"), 6);
        assert_eq!(display_width("日本"), 4);
        assert_eq!(display_width("a\u{200B}b"), 2);
        assert_eq!(display_width(""), 0);

        let mut table = Table::new(vec![
            Column::text("name", "Name"),
            Column::integer("n", "N"),
        ]);
        table.push(vec!["日本".into(), Cell::Integer(1)]);
        table.push(vec!["Zu\u{308}rich".into(), Cell::Integer(2)]);
        let text = rendered(&table, &options(Format::Table));
        let widths: Vec<usize> = text.lines().map(display_width).collect();
        assert_eq!(widths, vec![9, 9, 9]);
    }

    #[test]
    fn truncate_respects_the_width() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 5), "abcd…");
        assert_eq!(truncate("abcdef", 1), "…");
        assert_eq!(truncate("abcdef", 0), "");
        // A wide character that does not fit is dropped whole.
        assert_eq!(truncate("日本語", 4), "日…");
        assert_eq!(truncate("ab  cdef", 5), "ab…");
    }

    #[test]
    fn table_shows_decimals_with_the_scale_of_the_column() {
        let mut table = Table::new(vec![
            Column::decimal("quantity", "Quantity", 3),
            Column::new("average", "Average", Kind::Float),
            Column::new("at", "At", Kind::Timestamp),
        ]);
        table.push(vec![
            dec("1.5").into(),
            Cell::Float(0.25),
            Timestamp::from_micros(0).into(),
        ]);
        table.push(vec![
            dec("1200.12345").into(),
            Cell::Float(f64::NAN),
            Cell::Null,
        ]);
        let text = rendered(&table, &options(Format::Table));
        let expected = [
            "QUANTITY  AVERAGE  AT",
            "   1.500     0.25  1970-01-01 00:00:00",
            "1200.123      NaN",
        ]
        .join("\n")
            + "\n";
        assert_eq!(text, expected);
    }

    #[test]
    fn money_without_any_currency_has_no_gap() {
        let mut table = Table::new(vec![Column::money("amount", "Amount", "currency")]);
        table.push(vec![dec("1").into()]);
        table.push(vec![dec("-20.5").into()]);
        assert_eq!(
            rendered(&table, &options(Format::Table)),
            "AMOUNT\n  1.00\n-20.50\n"
        );
    }

    #[test]
    fn short_and_long_rows_do_not_panic() {
        let mut table = Table::new(vec![Column::integer("a", "A"), Column::text("b", "B")]);
        table.push(vec![Cell::Integer(1)]);
        table.push(vec![Cell::Integer(2), "x".into(), "extra".into()]);
        assert_eq!(rendered(&table, &options(Format::Table)), "A  B\n1\n2  x\n");
        assert_eq!(rendered(&table, &options(Format::Csv)), "a,b\n1,\n2,x\n");
        assert_eq!(
            rendered(&table, &options(Format::Jsonl)),
            "{\"a\":1,\"b\":null}\n{\"a\":2,\"b\":\"x\"}\n"
        );
    }

    // --- empty results ---

    #[test]
    fn empty_table_in_every_format() {
        let table = Table::new(vec![
            Column::integer("id", "Id"),
            Column::text("name", "Name"),
        ]);
        assert_eq!(rendered(&table, &options(Format::Table)), "");
        assert_eq!(rendered(&table, &options(Format::Json)), "[]\n");
        assert_eq!(rendered(&table, &options(Format::Jsonl)), "");
        assert_eq!(rendered(&table, &options(Format::Csv)), "id,name\n");
        let no_header = Options {
            header: false,
            ..options(Format::Csv)
        };
        assert_eq!(rendered(&table, &no_header), "");
    }

    // --- json ---

    #[test]
    fn json_is_one_array_with_stable_keys() {
        let text = rendered(&bookings(), &options(Format::Json));
        let expected = r#"[
  {"id":7,"booking_date":"2026-01-05","amount":-12.50,"currency":"EUR","payee":"Bakery","purpose":"Bread and rolls","pending":false},
  {"id":1234,"booking_date":"2026-01-31","amount":2500.00,"currency":"EUR","payee":"Employer Ltd","purpose":"Salary January\nThank you","pending":false},
  {"id":8,"booking_date":null,"amount":-0.999,"currency":null,"payee":null,"purpose":null,"pending":true}
]
"#;
        assert_eq!(text, expected);

        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        let rows = parsed.as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["amount"], serde_json::json!(-12.5));
        assert!(rows[2]["payee"].is_null());
    }

    #[test]
    fn jsonl_is_one_object_per_line() {
        let text = rendered(&bookings(), &options(Format::Jsonl));
        assert_eq!(text.lines().count(), 3);
        for line in text.lines() {
            let object: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(object.as_object().unwrap().len(), 7);
        }
    }

    #[test]
    fn json_escapes_strings() {
        assert_eq!(json_string("plain", false), "\"plain\"");
        assert_eq!(json_string("a\"b\\c", false), r#""a\"b\\c""#);
        assert_eq!(
            json_string("line\nbreak\ttab\r", false),
            r#""line\nbreak\ttab\r""#
        );
        assert_eq!(json_string("\x1b[0m\x00", false), r#""\u001b[0m\u0000""#);
        assert_eq!(json_string("Zürich 日本", false), "\"Zürich 日本\"");

        // Whatever goes in comes out of a JSON parser unchanged.
        let nasty = "quote\" backslash\\ \u{0}\u{1f}\u{7f}\u{2028} \u{1F600} end";
        let parsed: String = serde_json::from_str(&json_string(nasty, false)).unwrap();
        assert_eq!(parsed, nasty);
    }

    #[test]
    fn json_values_by_type() {
        let column = Column::text("x", "X");
        let json = |cell: Cell| json_value(&column, &cell, false);
        assert_eq!(json(Cell::Null), "null");
        assert_eq!(json(Cell::Integer(-42)), "-42");
        assert_eq!(json(Cell::Boolean(true)), "true");
        assert_eq!(json(Cell::Float(0.1)), "0.1");
        assert_eq!(json(Cell::Float(2.0)), "2");
        assert_eq!(json(Cell::Float(f64::INFINITY)), "\"Infinity\"");
        assert_eq!(json(Cell::Float(f64::NAN)), "\"NaN\"");
        assert_eq!(
            json(Timestamp::from_micros(0).into()),
            "\"1970-01-01T00:00:00\""
        );
        assert_eq!(json(Option::<i64>::None.into()), "null");
        assert_eq!(json(Some(5i64).into()), "5");
    }

    #[test]
    fn json_writes_an_integer_as_a_number_only_up_to_2_pow_53() {
        let column = Column::integer("n", "N");
        let json = |value: i64| json_value(&column, &Cell::Integer(value), false);
        let safe = 1_i64 << 53;
        assert_eq!(json(0), "0");
        assert_eq!(json(safe), "9007199254740992");
        assert_eq!(json(-safe), "-9007199254740992");
        assert_eq!(json(safe + 1), "\"9007199254740993\"");
        assert_eq!(json(-safe - 1), "\"-9007199254740993\"");
        assert_eq!(json(i64::MAX), "\"9223372036854775807\"");
        assert_eq!(json(i64::MIN), "\"-9223372036854775808\"");

        // The claim behind the rule: every number written reads back from a
        // double as the same integer.
        for value in [safe, -safe, safe - 1, 1, -1, 123_456_789_012_345] {
            let double: f64 = json(value).parse().unwrap();
            assert_eq!(double as i64, value);
            assert_eq!(double.to_string(), value.to_string());
        }
        // An integer beyond 64 bits is a decimal and follows its rule.
        let wide = Cell::Decimal(Decimal::from_i128_with_scale(9_223_372_036_854_775_808, 0));
        assert_eq!(json_value(&column, &wide, false), "\"9223372036854775808\"");

        // CSV has no such limit: the digits are the value.
        let mut table = Table::new(vec![column.clone()]);
        table.push(vec![Cell::Integer(i64::MAX)]);
        assert_eq!(
            rendered(&table, &options(Format::Csv)),
            "n\n9223372036854775807\n"
        );
        assert_eq!(
            rendered(&table, &options(Format::Json)),
            "[\n  {\"n\":\"9223372036854775807\"}\n]\n"
        );
    }

    // --- terminals ---

    /// Text as a sender of money could write it: clear the screen (ESC and
    /// the 8-bit CSI), reverse the line, ring the bell, overwrite the line.
    const HOSTILE: &str = "pay\x1b[2J\u{9b}2J\u{202e}evil\u{2066}\x07\x7f\rme";

    fn hostile_table() -> Table {
        let mut table = Table::new(vec![
            Column::integer("id", "Id"),
            Column::text("pur\x1bpose", "Pur\x1bpose"),
        ]);
        table.push(vec![Cell::Integer(1), HOSTILE.into()]);
        table.push(vec![Cell::Integer(2), "two\r\nlines\tand\nthree".into()]);
        table
    }

    fn rendered_on(table: &Table, format: Format, terminal: bool) -> String {
        let mut out = Vec::new();
        render_table_on(&mut out, table, &options(format), terminal).unwrap();
        String::from_utf8(out).unwrap()
    }

    /// Whether `text` holds anything a terminal would act on, apart from the
    /// line feeds and tabs of the layout.
    fn acts(text: &str) -> bool {
        text.chars()
            .any(|c| c != '\n' && c != '\t' && c != '\r' && acts_on_terminal(c))
            || text.replace("\r\n", "").contains('\r')
    }

    #[test]
    fn csv_is_exact_for_a_file_and_cleaned_for_a_terminal() {
        let table = hostile_table();
        // A file or a pipe gets the bytes of the value.
        assert_eq!(
            rendered_on(&table, Format::Csv, false),
            format!("id,pur\x1bpose\n1,\"{HOSTILE}\"\n2,\"two\r\nlines\tand\nthree\"\n")
        );
        assert_eq!(
            rendered_on(&table, Format::Csv, false),
            rendered(&table, &options(Format::Csv))
        );

        // A terminal gets U+FFFD for each character it would act on. The
        // line breaks and the tab stay: they are inside the quotes.
        let shown = rendered_on(&table, Format::Csv, true);
        assert_eq!(
            shown,
            "id,pur\u{fffd}pose\n\
             1,pay\u{fffd}[2J\u{fffd}2J\u{fffd}evil\u{fffd}\u{fffd}\u{fffd}\u{fffd}me\n\
             2,\"two\r\nlines\tand\nthree\"\n"
        );
        assert!(!acts(&shown));
    }

    #[test]
    fn json_is_exact_for_a_file_and_escaped_for_a_terminal() {
        let table = hostile_table();
        for format in [Format::Json, Format::Jsonl] {
            // A file gets the characters themselves, except those below
            // U+0020, which JSON does not allow in a string.
            let exact = rendered_on(&table, format, false);
            assert!(exact.contains("\\u001b[2J\u{9b}2J\u{202e}evil\u{2066}\\u0007\x7f\\rme"));
            assert_eq!(exact, rendered(&table, &options(format)));

            // A terminal gets escapes, and so nothing to act on.
            let shown = rendered_on(&table, format, true);
            assert!(
                shown.contains(
                    r#""pur\u001bpose":"pay\u001b[2J\u009b2J\u202eevil\u2066\u0007\u007f\rme""#
                ),
                "{shown}"
            );
            assert!(!acts(&shown), "{shown:?}");

            // Both are the same data.
            let parse = |text: &str| -> Vec<serde_json::Value> {
                if format == Format::Json {
                    serde_json::from_str(text).unwrap()
                } else {
                    text.lines()
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect()
                }
            };
            assert_eq!(parse(&exact), parse(&shown));
            assert_eq!(parse(&shown)[0]["pur\x1bpose"], serde_json::json!(HOSTILE));
        }
    }

    #[test]
    fn a_table_is_cleaned_wherever_it_goes() {
        let table = hostile_table();
        for terminal in [false, true] {
            let shown = rendered_on(&table, Format::Table, terminal);
            assert_eq!(
                shown,
                "ID  PUR\u{fffd}POSE\n \
                 1  pay\u{fffd}[2J\u{fffd}2J\u{fffd}evil\u{fffd}\u{fffd}\u{fffd} ↵ me\n \
                 2  two ↵ lines and ↵ three\n"
            );
            assert!(!acts(&shown));
        }
    }

    #[test]
    fn a_record_follows_the_same_rule() {
        let mut record = Record::new();
        record.push(Column::text("purpose", "Purpose"), HOSTILE);
        let render = |format: Format, terminal: bool| {
            let mut out = Vec::new();
            render_record_on(&mut out, &record, &options(format), terminal).unwrap();
            String::from_utf8(out).unwrap()
        };
        for format in [Format::Json, Format::Jsonl, Format::Csv] {
            assert!(acts(&render(format, false)) || format != Format::Csv);
            assert_eq!(
                render(format, false),
                rendered_record(&record, &options(format))
            );
            assert!(!acts(&render(format, true)), "{format:?}");
        }
        assert_eq!(
            render(Format::Csv, false),
            format!("purpose\n\"{HOSTILE}\"\n")
        );
        assert!(!acts(&render(Format::Table, false)));
    }

    #[test]
    fn any_json_text_can_be_escaped_for_a_terminal() {
        let value = serde_json::json!({ "a": HOSTILE, "b": ["x\u{85}y", 1.5, null] });
        let pretty = serde_json::to_string_pretty(&value).unwrap();
        let shown = json_for_terminal(&pretty);
        assert!(!acts(&shown), "{shown:?}");
        // The layout is kept and the document is the same.
        assert_eq!(shown.lines().count(), pretty.lines().count());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&shown).unwrap(),
            value
        );
        // A character beyond U+FFFF would take two escapes.
        let mut literal = String::new();
        push_json_escape(&mut literal, '\u{1F600}');
        assert_eq!(literal, "\\ud83d\\ude00");
    }

    #[test]
    fn messages_are_cleaned_but_keep_their_lines() {
        assert_eq!(
            message_text("Conversion Error: Could not convert string '\x1b[2J' to INT32\n\nLINE 1: select\t1"),
            "Conversion Error: Could not convert string '\u{fffd}[2J' to INT32\n\nLINE 1: select\t1"
        );
        assert_eq!(
            message_text("a\rb\u{9b}c\u{202e}d\u{2069}e\x00\x7f"),
            "a\u{fffd}b\u{fffd}c\u{fffd}d\u{fffd}e\u{fffd}\u{fffd}"
        );
        assert_eq!(message_text("Zürich 日本 ↵"), "Zürich 日本 ↵");
    }

    // --- results in chunks ---

    /// `table` written through a [`TableStream`] in chunks of `size` rows.
    fn streamed(table: &Table, options: &Options, terminal: bool, size: usize) -> String {
        let mut out = Vec::new();
        let mut stream = TableStream::new(&mut out, *options, terminal);
        let mut chunks = table.rows.chunks(size).peekable();
        if chunks.peek().is_none() {
            stream.write(Table::new(table.columns.clone())).unwrap();
        }
        for rows in chunks {
            let mut chunk = Table::new(table.columns.clone());
            chunk.rows = rows.to_vec();
            stream.write(chunk).unwrap();
        }
        assert_eq!(stream.finish().unwrap(), table.len() as u64);
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn a_result_in_chunks_is_written_like_the_whole() {
        let empty = Table::new(bookings().columns);
        for table in [bookings(), hostile_table(), empty] {
            for format in [Format::Table, Format::Json, Format::Jsonl, Format::Csv] {
                for header in [true, false] {
                    let options = Options {
                        header,
                        ..options(format)
                    };
                    for terminal in [false, true] {
                        let mut whole = Vec::new();
                        render_table_on(&mut whole, &table, &options, terminal).unwrap();
                        let whole = String::from_utf8(whole).unwrap();
                        for size in [1, 2, 100] {
                            assert_eq!(
                                streamed(&table, &options, terminal, size),
                                whole,
                                "{format:?}, chunks of {size}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn the_machine_formats_are_written_as_the_chunks_arrive() {
        let mut out = Vec::new();
        let mut stream = TableStream::new(&mut out, options(Format::Csv), false);
        let mut chunk = Table::new(vec![Column::integer("n", "N")]);
        chunk.push(vec![Cell::Integer(1)]);
        stream.write(chunk.clone()).unwrap();
        stream.write(chunk).unwrap();
        // Not waiting for `finish`.
        assert_eq!(stream.out.as_slice(), b"n\n1\n1\n");
        assert_eq!(stream.finish().unwrap(), 2);
    }

    #[test]
    fn a_table_too_large_to_buffer_is_refused_before_anything_is_written() {
        let chunk = |rows: usize, text: &str| {
            let mut chunk = Table::new(vec![Column::text("t", "T")]);
            chunk.rows = vec![vec![Cell::from(text)]; rows];
            chunk
        };
        let mut out = Vec::new();
        let mut stream = TableStream::new(&mut out, options(Format::Table), true);
        stream.write(chunk(MAX_TABLE_ROWS, "x")).unwrap();
        let error = stream.write(chunk(1, "x")).unwrap_err();
        assert_eq!(
            error.to_string(),
            "the result has more than 100000 rows, which is all the table format holds in \
             memory: use --output csv or --output jsonl, or add a LIMIT to the query"
        );
        assert_eq!(error.exit_code(), 1);
        drop(stream);
        assert!(out.is_empty());

        // The limit itself is fine, and so is any size in a streamed format.
        let mut stream = TableStream::new(Vec::new(), options(Format::Table), true);
        stream.write(chunk(MAX_TABLE_ROWS, "x")).unwrap();
        assert_eq!(stream.finish().unwrap(), MAX_TABLE_ROWS as u64);
        let mut stream = TableStream::new(io::sink(), options(Format::Jsonl), false);
        stream.write(chunk(MAX_TABLE_ROWS, "x")).unwrap();
        stream.write(chunk(MAX_TABLE_ROWS, "x")).unwrap();
        assert_eq!(stream.finish().unwrap(), 2 * MAX_TABLE_ROWS as u64);

        // A few rows of very long text are refused as well.
        let long = "x".repeat(MAX_TABLE_TEXT_BYTES / 4);
        let mut stream = TableStream::new(io::sink(), options(Format::Table), true);
        stream.write(chunk(4, &long)).unwrap();
        let error = stream.write(chunk(1, "y")).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("the result has more than 256 MiB of text"),
            "{error}"
        );
    }

    #[test]
    fn a_closed_pipe_ends_a_stream_quietly() {
        let mut stream = TableStream::new(ClosedPipe, options(Format::Csv), false);
        let error = stream.write(bookings()).unwrap_err();
        assert!(error.is_silent());
        assert_eq!(error.exit_code(), 0);
    }

    // --- csv ---

    #[test]
    fn csv_has_a_header_and_exact_values() {
        let text = rendered(&bookings(), &options(Format::Csv));
        let expected = "\
id,booking_date,amount,currency,payee,purpose,pending
7,2026-01-05,-12.50,EUR,Bakery,Bread and rolls,false
1234,2026-01-31,2500.00,EUR,Employer Ltd,\"Salary January
Thank you\",false
8,,-0.999,,,,true
";
        assert_eq!(text, expected);
        assert!(!text.contains('\r'));

        let no_header = Options {
            header: false,
            ..options(Format::Csv)
        };
        assert!(rendered(&bookings(), &no_header).starts_with("7,2026-01-05"));
    }

    #[test]
    fn csv_quotes_what_rfc_4180_requires() {
        let field = |text: &str| {
            let mut line = String::new();
            push_csv_field(&mut line, text);
            line
        };
        assert_eq!(field("plain"), "plain");
        assert_eq!(field("a,b"), "\"a,b\"");
        assert_eq!(field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(field("two\nlines"), "\"two\nlines\"");
        assert_eq!(field("cr\rhere"), "\"cr\rhere\"");
        assert_eq!(field(" padded "), "\" padded \"");
        assert_eq!(field("a;b"), "a;b");
        assert_eq!(field(""), "");
    }

    #[test]
    fn csv_tells_an_empty_text_from_a_missing_value() {
        let mut table = Table::new(vec![Column::text("a", "A"), Column::text("b", "B")]);
        table.push(vec!["".into(), Cell::Null]);
        assert_eq!(rendered(&table, &options(Format::Csv)), "a,b\n\"\",\n");
    }

    #[test]
    fn csv_keeps_all_digits_of_a_long_decimal() {
        let mut table = Table::new(vec![Column::decimal("price", "Price", 2)]);
        table.push(vec![dec("12345678901234567.8901234567").into()]);
        assert_eq!(
            rendered(&table, &options(Format::Csv)),
            "price\n12345678901234567.8901234567\n"
        );
    }

    // --- records ---

    fn booking_record() -> Record {
        let mut record = Record::new();
        record.push(Column::integer("id", "Id"), 42i64);
        record.push(
            Column::new("booking_date", "Booking date", Kind::Date),
            Date::new(2026, 3, 1),
        );
        record.push(Column::money("amount", "Amount", "currency"), dec("-7.5"));
        record.push(Column::text("currency", "Currency"), "EUR");
        record.push(
            Column::text("purpose", "Purpose"),
            "first line\nsecond \x1b[2Jline",
        );
        record.push(Column::text("notes", "Notes"), Cell::Null);
        record
    }

    #[test]
    fn record_as_aligned_keys_and_values() {
        let text = rendered_record(&booking_record(), &options(Format::Table));
        let expected = "\
Id:            42
Booking date:  2026-03-01
Amount:        -7.50 EUR
Currency:      EUR
Purpose:       first line
               second \u{FFFD}[2Jline
Notes:
";
        assert_eq!(text, expected);
    }

    #[test]
    fn record_as_json_object() {
        let text = rendered_record(&booking_record(), &options(Format::Json));
        let expected = r#"{
  "id": 42,
  "booking_date": "2026-03-01",
  "amount": -7.50,
  "currency": "EUR",
  "purpose": "first line\nsecond \u001b[2Jline",
  "notes": null
}
"#;
        assert_eq!(text, expected);
        let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["id"], serde_json::json!(42));

        assert_eq!(
            rendered_record(&Record::new(), &options(Format::Json)),
            "{}\n"
        );
    }

    #[test]
    fn record_as_jsonl_and_csv() {
        let record = booking_record();
        let line = rendered_record(&record, &options(Format::Jsonl));
        assert_eq!(line.lines().count(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["currency"], serde_json::json!("EUR"));

        // As data the escape sequence in the purpose is kept byte for byte.
        let row = "42,2026-03-01,-7.50,EUR,\"first line\nsecond \x1b[2Jline\",\n";
        let csv = rendered_record(&record, &options(Format::Csv));
        assert_eq!(
            csv,
            format!("id,booking_date,amount,currency,purpose,notes\n{row}")
        );
        let no_header = Options {
            header: false,
            ..options(Format::Csv)
        };
        assert_eq!(rendered_record(&record, &no_header), row);
    }

    // --- closed pipes ---

    /// A writer whose reader has gone away.
    struct ClosedPipe;

    impl Write for ClosedPipe {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
    }

    #[test]
    fn a_closed_pipe_ends_the_command_quietly() {
        for format in [Format::Table, Format::Json, Format::Jsonl, Format::Csv] {
            let error = render_table(&mut ClosedPipe, &bookings(), &options(format)).unwrap_err();
            let error = stdout_error(error);
            assert!(error.is_silent());
            assert_eq!(error.exit_code(), 0);
        }
    }

    #[test]
    fn another_write_error_is_reported() {
        let error = stdout_error(io::Error::other("disk full"));
        assert!(!error.is_silent());
        assert_eq!(error.exit_code(), 1);
        assert_eq!(
            error.to_string(),
            "cannot write to standard output: disk full"
        );
    }
}
