//! The command-line interface.
//!
//! This clap command is the single source of truth: the `b4` binary parses it,
//! and the build-time `b4-gen-assets` helper renders the man pages and the
//! shell completions from the very same definition (via
//! [`clap::CommandFactory`]), so `--help`, the man pages and the completions
//! cannot drift apart.
//!
//! The grammar is `b4 <resource> <verb>` for everything that lists or shows
//! data, and a single word for what acts on the vault or on the database as a
//! whole. The argument structs of all commands live here; `commands::<name>`
//! receives the parsed struct of its command and the [`Globals`].
//!
//! There is no `--password` option, on purpose (see [`crate::password`]), and a
//! test keeps it that way.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use clap::builder::styling::Styles;
use clap::builder::NonEmptyStringValueParser;
use clap::ValueHint;
use clap::{Args, ColorChoice, CommandFactory, FromArgMatches, Parser, Subcommand, ValueEnum};
use rust_decimal::Decimal;

use crate::error::{stderr_colour, ERROR_STYLE};
use crate::output::{days_in_month, Date};

/// Help and usage errors are plain text; only the `error:` prefix has a
/// colour, the same one as for errors at run time.
const STYLES: Styles = Styles::plain().error(ERROR_STYLE);

/// Heading of the options every command accepts.
const GLOBAL_OPTIONS: &str = "Global options";

/// Heading of the options that select bookings.
const FILTERS: &str = "Filters";

/// Read Banking4 vault files: decrypt a vault into a DuckDB database and query it
///
/// b4 works in two steps. `b4 decrypt` reads a Banking4 vault (.sub), asks for
/// its password once and writes the data into a DuckDB database file. Every
/// other command reads only that database and never needs the password, so
/// scripts and agents can query it. To "lock" the data again, delete the
/// database file; to refresh it, run `b4 decrypt` again.
///
/// The database is the file given with --db, else the one named by the
/// environment variable B4_DB, else ./b4.duckdb.
///
/// A vault is only ever read, never changed. Bank access data stored in a vault
/// (logins, keys, TAN settings) is never read and never reaches the database.
#[derive(Debug, Parser)]
#[command(
    name = "b4",
    version,
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true,
    styles = STYLES,
    after_long_help = "\
Examples:
  b4 decrypt ~/Banking/Household.sub
      Ask for the vault password and write ./b4.duckdb.
  b4 accounts list
      List the accounts in the database.
  b4 bookings list --account giro --from 2025-01 --filter supermarkt
      Search the bookings of one account since January 2025.
  b4 bookings summary --by category --from 2025 --output json
      Sum up the bookings since 2025 per category, as JSON.
  b4 sql 'select count(*) from bookings'
      Run any read-only SQL query.
  rm b4.duckdb
      Lock again: the decrypted copy is gone, the vault is untouched.

Environment:
  B4_DB        The database file, as with --db.
  B4_PASSWORD  The vault password for `b4 decrypt`. Other processes of the
               same user can read it; `b4 decrypt --help` names better ways.
  NO_COLOR     When set, the `error:` prefix is not coloured."
)]
pub struct Cli {
    /// Options that every command accepts.
    #[command(flatten)]
    pub globals: Globals,

    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

impl Cli {
    /// Parse the arguments of the process. Prints help, the version or a usage
    /// error and exits (status 0 or 2) when there is no command to run.
    ///
    /// Unlike `Cli::parse()`, this applies b4's one colour rule to clap's own
    /// messages too: the `error:` prefix is red only if standard error is a
    /// terminal and `NO_COLOR` is not set, whatever else the environment says.
    pub fn from_command_line() -> Cli {
        let choice = if stderr_colour() {
            ColorChoice::Always
        } else {
            ColorChoice::Never
        };
        let mut command = Cli::command().color(choice);
        let matches = command.get_matches_mut();
        match Cli::from_arg_matches(&matches) {
            Ok(cli) => cli,
            Err(error) => error.format(&mut command).exit(),
        }
    }

    /// Check what clap cannot express: ranges whose ends are separate options.
    /// The message of a failed check is a usage error.
    pub(crate) fn validate(&self) -> Result<(), String> {
        match &self.command {
            Command::Bookings(BookingsCommand::List(args)) => args.filters.validate(),
            Command::Bookings(BookingsCommand::Summary(args)) => args.filters.validate(),
            _ => Ok(()),
        }
    }
}

/// The options every command accepts, before or after the command name.
#[derive(Debug, Clone, Args)]
pub struct Globals {
    /// Database file: written by `decrypt`, read by every other command
    #[arg(
        long,
        global = true,
        env = "B4_DB",
        value_name = "PATH",
        default_value = "./b4.duckdb",
        value_parser = parse_path,
        value_hint = ValueHint::FilePath,
        help_heading = GLOBAL_OPTIONS
    )]
    pub db: PathBuf,

    /// Output format [default: table]
    ///
    /// table is made for reading: line breaks, control characters and text
    /// direction overrides in the data are replaced. csv, jsonl and json are
    /// data: written to a file or a pipe they are exact, byte for byte. Only
    /// when they are written to a terminal are the characters a terminal acts
    /// on (escape sequences, text direction overrides) made harmless: csv shows
    /// U+FFFD in their place, json and jsonl write them as \uXXXX escapes.
    ///
    /// In json and jsonl a number that a double cannot hold exactly is a
    /// string of its digits: an integer beyond 2^53, a decimal of more than 15
    /// significant digits.
    #[arg(
        short = 'o',
        long,
        global = true,
        value_enum,
        value_name = "FORMAT",
        help_heading = GLOBAL_OPTIONS
    )]
    pub output: Option<Format>,

    /// Leave out the header line of table and csv output
    #[arg(long, global = true, help_heading = GLOBAL_OPTIONS)]
    pub no_header: bool,

    /// Do not print informational messages to stderr
    ///
    /// Silences progress and summary lines and the "no rows" note. Warnings and
    /// errors are still printed.
    #[arg(short = 'q', long, global = true, help_heading = GLOBAL_OPTIONS)]
    pub quiet: bool,
}

impl Globals {
    /// The output format: the one given with `--output`, else `table`.
    ///
    /// `output` itself stays `None` when the option was not given, for the
    /// one command that must tell `--output table` from no `--output`
    /// (`b4 export`).
    pub fn format(&self) -> Format {
        self.output.unwrap_or(Format::Table)
    }
}

/// The output formats of `--output`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Aligned text, fitted to the terminal
    Table,
    /// One JSON array of objects
    Json,
    /// One JSON object per line
    Jsonl,
    /// Comma-separated values with a header line
    Csv,
}

/// The commands of b4.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Decrypt a vault into a DuckDB database
    ///
    /// Decrypt VAULT and write its accounts, bookings, payees, categories,
    /// standing orders, payments and securities data into a DuckDB database.
    ///
    /// The database is written to a temporary file next to its final place and
    /// renamed when it is complete, so a reader never sees half a database. An
    /// existing database is replaced: running decrypt again is how the database is
    /// refreshed. The file is created with mode 0600.
    ///
    /// The password comes from the first of these that applies: standard input
    /// with --password-stdin, the environment variable B4_PASSWORD, a prompt
    /// without echo on the terminal. It cannot be given as an argument and is
    /// never stored.
    ///
    /// Prefer the prompt or --password-stdin. An environment variable can be
    /// read by every other process of the same user while b4 runs, is passed
    /// on to the programs a shell starts, and stays set in the shell that
    /// exported it. b4 removes B4_PASSWORD from its own environment as soon as
    /// it has read it, but cannot hide what it was started with.
    ///
    /// One line per table and any warnings go to stderr. The vault is opened
    /// read-only, and the bank access data in it is skipped without being read.
    Decrypt(DecryptArgs),

    /// Show a vault's header without the password
    ///
    /// Show what a vault says about itself in its unencrypted header: format
    /// version, cipher, number of key-stretching rounds, cluster size, vault id,
    /// creation and modification time, and the file size.
    ///
    /// No password is needed and nothing is decrypted.
    Info(InfoArgs),

    /// Describe the database and whether it is stale
    ///
    /// Show where the database is and what is in it: path and size, the b4 and
    /// schema version that wrote it, the vault it came from (path, id and
    /// modification stamp at decrypt time), when it was decrypted, the number of
    /// rows per table, and the warnings recorded while decrypting.
    ///
    /// If the vault is still readable at its recorded path, it is compared with
    /// what was recorded when it was decrypted: the vault id, the modification
    /// stamp in its header, the size of the file and, when these three are the
    /// same, a checksum of its content. Status says "up to date", "vault
    /// changed since decrypt", "another vault is at that path now" or "vault
    /// not reachable", and names the comparisons it made. No password is
    /// needed; the file is only read in full for the checksum.
    ///
    /// A database that does not record one of these values is never called up
    /// to date: status says "database metadata incomplete", and `b4 decrypt`
    /// writes the database again.
    Status(StatusArgs),

    /// List and show accounts
    #[command(
        subcommand,
        after_long_help = "\
Examples:
  b4 accounts list
      List the visible accounts.
  b4 accounts show giro
      Show the one account whose name contains \"giro\"."
    )]
    Accounts(AccountsCommand),

    /// List, show and sum up bookings
    #[command(
        subcommand,
        after_long_help = "\
Examples:
  b4 bookings list --from 2025-03 --to 2025-03
      Everything booked in March 2025.
  b4 bookings show 796370400
      Every stored field of one booking.
  b4 bookings summary --by category --from 2025
      The totals per category since 2025."
    )]
    Bookings(BookingsCommand),

    /// List payees
    #[command(
        subcommand,
        after_long_help = "\
Examples:
  b4 payees list --filter versicherung
      Payees with \"versicherung\" in one of their fields."
    )]
    Payees(PayeesCommand),

    /// List categories
    #[command(
        subcommand,
        after_long_help = "\
Examples:
  b4 categories list
      Show the category tree."
    )]
    Categories(CategoriesCommand),

    /// List standing orders
    #[command(
        subcommand,
        after_long_help = "\
Examples:
  b4 standing-orders list --account giro
      The standing orders of one account."
    )]
    StandingOrders(StandingOrdersCommand),

    /// List securities positions
    #[command(
        subcommand,
        after_long_help = "\
Examples:
  b4 portfolio list
      List the positions of all securities accounts."
    )]
    Portfolio(PortfolioCommand),

    /// Run a read-only SQL query against the database
    ///
    /// Run one SQL query against the database and print the result in the chosen
    /// output format. `b4 schema` lists the tables and columns.
    ///
    /// The query can only read this database. The connection is read-only and
    /// cannot reach anything else: no other files, no ATTACH, no extensions, no
    /// network, and no change of these settings. That makes it safe to run a query
    /// written by someone else, or by an agent. The switches of the database
    /// engine that are not settings (PRAGMA enable_profiling, enable_logging,
    /// enable_progress_bar and the like) and the functions query() and
    /// write_log() are refused as well.
    ///
    /// A query may use up to 1 GiB of memory and cannot use the disk; one that
    /// needs more fails. csv, jsonl and json are written while the result
    /// arrives, however large it is. table has to hold the whole result to
    /// size its columns and stops at 100000 rows: use another format or a
    /// LIMIT for more. A query on standard input may be up to 1 MiB long.
    Sql(SqlArgs),

    /// List tables and columns with their descriptions
    ///
    /// List the tables of the database with their descriptions, or the columns of
    /// one table with types and descriptions. The descriptions are stored in the
    /// database itself, so they always match the version of b4 that wrote it.
    Schema(SchemaArgs),

    /// Export tables to files, one file per table
    ///
    /// Write the tables of the database into DIR, one file per table, named after
    /// the table (bookings.csv, accounts.csv, ...). DIR is created if it does not
    /// exist; existing files of the same name are replaced.
    ///
    /// The files hold the exact values. The format is chosen with --format, or
    /// with the global --output when that is csv or jsonl. A table whose name
    /// cannot be a file name on every system (a path separator, a colon, a
    /// device name of Windows such as NUL) is not exported.
    Export(ExportArgs),

    /// Print a shell completion script
    ///
    /// Print a completion script for the given shell to standard output. Packages
    /// install the scripts already; this is for a b4 that was installed by hand.
    Completions(CompletionsArgs),
}

// --- decrypt, info, status ---------------------------------------------------

/// Arguments of `b4 decrypt`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 decrypt ~/Banking/Household.sub
      Ask for the password and write ./b4.duckdb.
  b4 decrypt ~/Banking/Household.sub ~/.cache/b4/household.duckdb
      Write the database somewhere else.
  pass show banking4 | b4 decrypt --password-stdin ~/Banking/Household.sub
      Take the password from a password manager.
  B4_DB=~/.cache/b4.duckdb b4 decrypt ~/Banking/Household.sub
      Refresh the database that the other commands use.")]
pub struct DecryptArgs {
    /// The vault file (.sub) to read
    #[arg(value_name = "VAULT", value_parser = parse_path, value_hint = ValueHint::FilePath)]
    pub vault: PathBuf,

    /// The database file to write [default: the file of --db]
    #[arg(value_name = "OUT", value_parser = parse_path, value_hint = ValueHint::FilePath)]
    pub out: Option<PathBuf>,

    /// Read the password from the first line of standard input
    ///
    /// One line ending (LF or CRLF) is removed, nothing else. An empty line
    /// is an empty password.
    #[arg(long)]
    pub password_stdin: bool,
}

impl DecryptArgs {
    /// The database file to write: OUT if given, else the file of `--db`.
    pub fn target<'a>(&'a self, globals: &'a Globals) -> &'a Path {
        self.out.as_deref().unwrap_or(&globals.db)
    }
}

/// Arguments of `b4 info`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 info ~/Banking/Household.sub
      Show the header of a vault.
  b4 info ~/Banking/Household.sub --output json
      The same as a JSON object.")]
pub struct InfoArgs {
    /// The vault file (.sub) to inspect
    #[arg(value_name = "VAULT", value_parser = parse_path, value_hint = ValueHint::FilePath)]
    pub vault: PathBuf,
}

/// Arguments of `b4 status`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 status
      Describe ./b4.duckdb.
  b4 status --check || b4 decrypt ~/Banking/Household.sub
      Decrypt again only when the vault has changed.")]
pub struct StatusArgs {
    /// Exit with status 1 unless the database is up to date
    ///
    /// That is: if the vault changed, another vault is at its path, the vault
    /// cannot be reached, or the database does not record what the comparison
    /// needs.
    #[arg(long)]
    pub check: bool,
}

// --- accounts ----------------------------------------------------------------

/// The verbs of `b4 accounts`.
#[derive(Debug, Clone, Subcommand)]
pub enum AccountsCommand {
    /// List accounts
    ///
    /// List the accounts in the order Banking4 shows them: id, type, name, IBAN
    /// (the account number where there is no IBAN), bank (BIC, else bank code),
    /// balance with its currency and the day it is from, and the number of
    /// bookings.
    ///
    /// Accounts that are hidden in Banking4 are left out unless --all is given.
    ///
    /// The number of bookings counts every booking once: a split booking or a
    /// collective booking counts as its parts, not as parts plus the whole.
    ///
    /// The id in the first column names the account wherever a command takes an
    /// ACCOUNT.
    ///
    /// With --output json, jsonl or csv every account has these fields: id, kind,
    /// name, iban, account_number, bic, bank_code, currency, balance, balance_at,
    /// hidden, booking_count, first_booking_date, last_booking_date.
    List(AccountsListArgs),

    /// Show everything about one account
    ///
    /// Show one account: every stored field (`b4 schema accounts` describes
    /// them), then the number of its bookings and how many of them are pending,
    /// the dates of the first and the last booking, and the sums of its credits
    /// and debits with the net amount.
    ///
    /// The counts and sums take every booking once (a split booking or a
    /// collective booking counts as its parts) and include pending bookings.
    /// They are the numbers of `b4 bookings summary --by account`.
    ///
    /// Table output leaves out fields without a value; json, jsonl and csv have
    /// every field. An account that is hidden in Banking4 can be shown too.
    Show(AccountsShowArgs),
}

/// Arguments of `b4 accounts list`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 accounts list
      List the visible accounts.
  b4 accounts list --all --type bank --type credit-card
      Bank and credit card accounts, hidden ones included.
  b4 accounts list --output csv > accounts.csv
      Write the list to a CSV file.")]
pub struct AccountsListArgs {
    /// Only accounts of this type; repeat for several types
    #[arg(long = "type", value_enum, value_name = "TYPE")]
    pub types: Vec<AccountType>,

    /// Include accounts that are hidden in Banking4
    #[arg(long)]
    pub all: bool,
}

/// Arguments of `b4 accounts show`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 accounts show giro
      Show the one account whose name contains \"giro\".
  b4 accounts show 'DE31 9999 0001 1000 4711 00'
      Show the account with this IBAN.
  b4 accounts show 757073100 --output json
      Show the account with this id as a JSON object.")]
pub struct AccountsShowArgs {
    /// The account: id, IBAN, account number or part of the name
    #[arg(value_name = "ACCOUNT", value_parser = NonEmptyStringValueParser::new(), long_help = ACCOUNT_HELP)]
    pub account: String,
}

/// How an ACCOUNT is resolved (`commands::account_ref`). The same sentence
/// follows the first line of the help of every argument that takes one.
macro_rules! account_rule {
    () => {
        "ACCOUNT is tried in this order: the numeric id from `b4 accounts list`; the IBAN \
(blanks and case do not matter); the account number; a part of the account name (case \
does not matter) that only one account has, where a complete name wins over a part. If \
several accounts match, the command fails and lists them with their ids."
    };
}

/// Long help of the ACCOUNT argument of `accounts show`.
const ACCOUNT_HELP: &str = concat!(
    "The account: id, IBAN, account number or part of the name\n\n",
    account_rule!()
);

/// The account types of `b4 accounts list --type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AccountType {
    /// Bank account
    Bank,
    /// Cash
    Cash,
    /// Credit card
    CreditCard,
    /// Securities account
    Portfolio,
    /// Assets such as real estate
    Asset,
    /// PayPal account
    Paypal,
    /// Crypto currency wallet
    Crypto,
}

impl AccountType {
    /// The name of this type in the `kind` column of the `accounts` table
    /// (03, "Account kind").
    pub fn kind_name(self) -> &'static str {
        match self {
            AccountType::Bank => "bank",
            AccountType::Cash => "cash",
            AccountType::CreditCard => "credit_card",
            AccountType::Portfolio => "portfolio",
            AccountType::Asset => "asset",
            AccountType::Paypal => "paypal",
            AccountType::Crypto => "crypto",
        }
    }
}

// --- bookings ----------------------------------------------------------------

/// The verbs of `b4 bookings`.
#[derive(Debug, Clone, Subcommand)]
pub enum BookingsCommand {
    /// List bookings
    ///
    /// List bookings, by default all of them ordered by date with the newest
    /// last. The filters narrow the list down; a booking must pass all of them.
    /// Bookings of accounts that are hidden in Banking4 are listed like any other.
    ///
    /// The table shows id, booking date, account, payee, purpose, category and
    /// the amount with its currency. Amounts are signed: negative is money
    /// leaving the account. Payee, purpose and category are cut to fit the
    /// terminal, never wrapped. A pending booking has a * after its date, or
    /// "pending" in its place as long as it has no booking date. Such a booking
    /// sorts after all dated ones.
    ///
    /// DATE is a day (2026-03-14), a month (2026-03) or a year (2026). As --from,
    /// a month or year means its first day; as --to, its last day.
    ///
    /// AMOUNT is a number with an optional decimal part after a point or a comma
    /// (1234.56 or 1234,56), without a sign and without thousands separators.
    ///
    /// A split booking and a collective booking are stored as one row for the
    /// whole and one row for each part, and all of these rows are listed. To add
    /// up amounts without counting money twice, use `b4 bookings summary`, or
    /// take only the rows whose field `counted` is true.
    ///
    /// With --output json, jsonl or csv every booking has these fields, named as
    /// in `b4 schema bookings_view`: account_id, id, account_name, account_iban,
    /// booking_date, value_date, status, pending, amount, currency, debit,
    /// payee_name, payee_iban, payee_bic, purpose, category, booking_text, notes,
    /// counted, split_role, master_id, end_to_end_id, mandate_id, creditor_id.
    /// `b4 bookings show` has the remaining fields of a booking.
    List(BookingsListArgs),

    /// Show every stored field of one booking
    ///
    /// Show every stored field of one booking, including the ones `bookings list`
    /// has no room for: value date, SEPA references, mandate, notes and the
    /// complete purpose text. `b4 schema bookings` describes the fields.
    ///
    /// An id is unique within one account only. If several accounts have a
    /// booking with the id, all of them are shown, each with its account; give
    /// --account to pick one.
    ///
    /// Table output leaves out fields without a value. With --output json the
    /// result is one object with every field, or an array of such objects when
    /// several bookings have the id; jsonl prints one object and csv one row per
    /// booking.
    Show(BookingsShowArgs),

    /// Count and sum up bookings by month, year, category, payee or account
    ///
    /// Group the bookings that pass the filters and print one line per group with
    /// the number of bookings, the sum of the debits (negative), the sum of the
    /// credits and the net amount, which is the two added up.
    ///
    /// Every booking counts once: of a split booking or a collective booking the
    /// parts are counted and the row for the whole is not. Amounts in different
    /// currencies are never added: a group with two currencies has two lines.
    /// Pending bookings are included; add --booked to leave them out.
    ///
    /// Months and years are those of the booking date, in ascending order;
    /// bookings without a booking date (pending ones) form a last group of their
    /// own. Categories, payees and accounts are ordered by name. A category
    /// stands for itself: the line of `Wohnen` does not include `Wohnen:Miete`
    /// (use --category Wohnen with --by year for the total of the subtree).
    /// Bookings without a category or without a payee form the last group.
    ///
    /// The filters are those of `b4 bookings list`. DATE is a day (2026-03-14), a
    /// month (2026-03) or a year (2026); AMOUNT is a number like 1234.56 or
    /// 1234,56.
    ///
    /// With --output json, jsonl or csv every line has the fields currency,
    /// booking_count, debits, credits and net after its group: month (YYYY-MM),
    /// year (YYYY), category, payee_name, or account_id and account_name. The
    /// group without a value is null there, and "(no date)" or "(none)" in the
    /// table.
    Summary(BookingsSummaryArgs),
}

/// Arguments of `b4 bookings list`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 bookings list --from 2025-03 --to 2025-03
      Everything booked in March 2025.
  b4 bookings list --account giro --filter supermarkt --debit
      Money spent from one account where any text field contains
      \"supermarkt\".
  b4 bookings list --category Wohnen --min 100 --sort amount
      Bookings of 100 or more in a category and its subcategories, the
      largest debit first.
  b4 bookings list --uncategorized --from 2025 --output csv
      The bookings since 2025 that have no category, as CSV.
  b4 bookings list --reverse --limit 20
      The 20 most recent bookings, newest first.")]
pub struct BookingsListArgs {
    /// Which bookings to list.
    #[command(flatten)]
    pub filters: BookingFilters,

    /// Order by date, amount or payee name
    #[arg(long, value_enum, value_name = "KEY", default_value_t = BookingSort::Date)]
    pub sort: BookingSort,

    /// Reverse the order: newest, largest or last in the alphabet first
    #[arg(long)]
    pub reverse: bool,

    /// Print at most N bookings, the first N in the chosen order
    #[arg(long, value_name = "N", value_parser = parse_limit)]
    pub limit: Option<u64>,
}

/// The options that select bookings, shared by `bookings list` and `bookings
/// summary`. A booking must pass every filter that is given.
#[derive(Debug, Clone, Default, Args)]
pub struct BookingFilters {
    /// Only bookings of this account; repeat for several accounts
    #[arg(
        long = "account",
        value_name = "ACCOUNT",
        value_parser = NonEmptyStringValueParser::new(),
        help_heading = FILTERS,
        long_help = concat!(
            "Only bookings of this account; repeat for several accounts\n\n",
            account_rule!()
        )
    )]
    pub accounts: Vec<String>,

    /// Only bookings on or after this date (booking date)
    #[arg(long, value_name = "DATE", help_heading = FILTERS)]
    pub from: Option<DateArg>,

    /// Only bookings on or before this date (booking date)
    #[arg(long, value_name = "DATE", help_heading = FILTERS)]
    pub to: Option<DateArg>,

    /// Only bookings with TEXT in any text field, ignoring case
    ///
    /// Searched are the payee name, the purpose, the booking text, the
    /// category, the notes and the payee's IBAN. TEXT is taken as it is: there
    /// are no wildcards. A word that the bank broke over two lines of the
    /// purpose is found as well.
    #[arg(long, value_name = "TEXT", value_parser = NonEmptyStringValueParser::new(), help_heading = FILTERS)]
    pub filter: Option<String>,

    /// Only bookings whose payee name contains TEXT, ignoring case
    #[arg(long, value_name = "TEXT", value_parser = NonEmptyStringValueParser::new(), help_heading = FILTERS)]
    pub payee: Option<String>,

    /// Only bookings in a category whose name, or the name of one of its
    /// parents, contains TEXT, ignoring case
    ///
    /// TEXT is searched in the full name of the category, which holds every
    /// level: `Wohnen` selects `Wohnen`, `Wohnen:Miete` and
    /// `Wohnen:Nebenkosten:Strom`; `Wohnen:Miete` selects only that one.
    #[arg(long, value_name = "TEXT", value_parser = NonEmptyStringValueParser::new(), help_heading = FILTERS)]
    pub category: Option<String>,

    /// Only bookings without a category
    #[arg(long, conflicts_with = "category", help_heading = FILTERS)]
    pub uncategorized: bool,

    /// Only bookings of at least this amount, whatever the direction
    #[arg(long, value_name = "AMOUNT", value_parser = parse_amount, help_heading = FILTERS)]
    pub min: Option<Decimal>,

    /// Only bookings of at most this amount, whatever the direction
    #[arg(long, value_name = "AMOUNT", value_parser = parse_amount, help_heading = FILTERS)]
    pub max: Option<Decimal>,

    /// Only money leaving the account
    #[arg(long, conflicts_with = "credit", help_heading = FILTERS)]
    pub debit: bool,

    /// Only money arriving on the account
    #[arg(long, help_heading = FILTERS)]
    pub credit: bool,

    /// Only bookings the bank has announced but not booked yet
    ///
    /// Pending bookings are not part of the balance of their account.
    #[arg(long, conflicts_with = "booked", help_heading = FILTERS)]
    pub pending: bool,

    /// Only bookings the bank has booked
    #[arg(long, help_heading = FILTERS)]
    pub booked: bool,
}

impl BookingFilters {
    /// The first day a booking may have, from `--from`.
    pub fn first_day(&self) -> Option<Date> {
        self.from.map(DateArg::first_day)
    }

    /// The last day a booking may have, from `--to`.
    pub fn last_day(&self) -> Option<Date> {
        self.to.map(DateArg::last_day)
    }

    /// Reject ranges that are the wrong way round. They would select nothing,
    /// and a silent empty result hides the typing error.
    fn validate(&self) -> Result<(), String> {
        if let (Some(from), Some(to)) = (self.first_day(), self.last_day()) {
            if from > to {
                return Err(format!("--from ({from}) is after --to ({to})"));
            }
        }
        if let (Some(min), Some(max)) = (self.min, self.max) {
            if min > max {
                return Err(format!("--min ({min}) is greater than --max ({max})"));
            }
        }
        Ok(())
    }
}

/// The sort keys of `b4 bookings list --sort`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BookingSort {
    /// Booking date, oldest first
    Date,
    /// Signed amount, smallest first
    Amount,
    /// Payee name, A to Z
    Payee,
}

/// Arguments of `b4 bookings show`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 bookings show 796370400
      Show the booking with this id.
  b4 bookings show 796370400 --account giro
      Only the booking with this id in one account.
  b4 bookings show 796370400 --output json
      The same as a JSON object with every field.")]
pub struct BookingsShowArgs {
    /// The id of the booking, as printed by `b4 bookings list`
    #[arg(value_name = "ID", allow_negative_numbers = true)]
    pub id: i64,

    /// Only the booking of this account
    #[arg(long, value_name = "ACCOUNT", value_parser = NonEmptyStringValueParser::new(), long_help = SHOW_ACCOUNT_HELP)]
    pub account: Option<String>,
}

/// Long help of `bookings show --account`.
const SHOW_ACCOUNT_HELP: &str = concat!(
    "Only the booking of this account\n\n",
    "An id is unique within one account only. ",
    account_rule!()
);

/// Arguments of `b4 bookings summary`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 bookings summary --by month --from 2025 --to 2025
      Income and spending per month of 2025.
  b4 bookings summary --by category --account giro --from 2025-01 --to 2025-06
      Totals per category for one account in the first half of 2025.
  b4 bookings summary --by year --category Wohnen --booked
      What a category and its subcategories cost per year, pending bookings
      left out.
  b4 bookings summary --by payee --debit --output json
      What was paid to whom, as JSON.")]
pub struct BookingsSummaryArgs {
    /// What to group the bookings by
    #[arg(long, value_enum, value_name = "GROUP")]
    pub by: SummaryBy,

    /// Which bookings to sum up.
    #[command(flatten)]
    pub filters: BookingFilters,
}

/// The groupings of `b4 bookings summary --by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum SummaryBy {
    /// Calendar month of the booking date
    Month,
    /// Calendar year of the booking date
    Year,
    /// Category
    Category,
    /// Payee name
    Payee,
    /// Account
    Account,
}

// --- payees, categories, standing orders, portfolio --------------------------

/// The verbs of `b4 payees`.
#[derive(Debug, Clone, Subcommand)]
pub enum PayeesCommand {
    /// List payees
    ///
    /// List the payees saved in the vault's address book, ordered by name
    /// (ignoring case; letters outside A to Z, such as umlauts, come after Z).
    ///
    /// Table output shows name, IBAN, BIC, bank, town and the number of bookings
    /// of each payee. The other formats carry every column of the `payees` table
    /// under its own name (see `b4 schema payees`), followed by `booking_count`.
    ///
    /// Bookings do not refer to an address book entry; they only carry the name
    /// of the other party. `booking_count` is therefore a match by name: the number
    /// of bookings whose payee name equals the payee's name, ignoring case and
    /// surrounding blanks. Payees that share a name share the number, and a
    /// booking that spells the name differently is not counted. Every booking
    /// counts once: a split booking or a collective booking counts as its parts,
    /// not as parts plus the whole. Pending bookings are included.
    List(PayeesListArgs),
}

/// Arguments of `b4 payees list`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 payees list
      List all saved payees.
  b4 payees list --filter versicherung
      Payees with \"versicherung\" in one of their fields.
  b4 payees list --filter 'DE49 9999'
      Payees whose IBAN contains DE499999.
  b4 payees list --output csv > payees.csv
      Every stored field of every payee, as CSV.")]
pub struct PayeesListArgs {
    /// Only payees with TEXT in one of their text fields, ignoring case
    ///
    /// Keeps the payees that have TEXT as a substring of their name, IBAN, BIC,
    /// account number, bank code, address, bank name or address, default
    /// purpose, default category, e-mail address, customer number or notes.
    /// Upper and lower case are the same. TEXT is taken literally: `%` and `_`
    /// are ordinary characters. An IBAN is also found when TEXT has blanks in
    /// it.
    #[arg(long, value_name = "TEXT", value_parser = NonEmptyStringValueParser::new())]
    pub filter: Option<String>,
}

/// The verbs of `b4 categories`.
#[derive(Debug, Clone, Subcommand)]
pub enum CategoriesCommand {
    /// List categories
    ///
    /// List the category catalogue as a tree, with the number and the sum of the
    /// bookings in each category.
    ///
    /// The levels of a category are the parts of its name between colons:
    /// `Wohnen:Nebenkosten:Strom` is `Strom` below `Nebenkosten` below `Wohnen`.
    /// Table output indents each level by two blanks. A parent that is no
    /// category of its own, only the beginning of another name, is listed with
    /// the note `no record`. Categories of one level are ordered by name,
    /// ignoring case, character by character in Unicode order: digits and
    /// punctuation come before letters, and letters outside A to Z, such as
    /// umlauts, after Z.
    ///
    /// BOOKINGS and SUM are the bookings of the category itself and their net
    /// sum (credits minus debits); WITH CHILDREN and SUM WITH CHILDREN include
    /// all categories below it. Every booking counts once: a split booking or a
    /// collective booking counts as its parts, not as parts plus the whole.
    /// Pending bookings are included. Sums are per currency: a category with
    /// bookings in several currencies has one line for each. A booking belongs
    /// to the category of the same name, ignoring case.
    ///
    /// Bookings in a category that is not in the catalogue follow under
    /// `(not in catalogue)`, one entry per name (ignoring case), and bookings
    /// without a category under `(uncategorized)`. BOOKINGS over all lines is
    /// therefore the number of all bookings that count.
    ///
    /// The other formats have one row per category and currency, with these
    /// columns: `full_name`, `name` (the last level), `parent` (the full name of
    /// the parent), `depth` (0 at the top), `kind` (`category`, `implied_parent`
    /// for a parent that is no category of its own, `not_in_catalogue` or
    /// `uncategorized`), `currency`, `booking_count`, `sum`,
    /// `booking_count_with_children`, `sum_with_children`, and from the
    /// `categories` table `budget_period`, `budget_amount`, `special`,
    /// `totals_bucket` and `hidden_by_banking4`. A category without bookings has
    /// one row with no currency and no sums.
    List(CategoriesListArgs),
}

/// Arguments of `b4 categories list`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 categories list
      Show the category tree with counts and sums.
  b4 categories list --output jsonl
      One JSON object per category and currency.
  b4 categories list --output csv | grep ',uncategorized,'
      The number and the sum of the bookings without a category.")]
pub struct CategoriesListArgs {}

/// The verbs of `b4 standing-orders`.
#[derive(Debug, Clone, Subcommand)]
pub enum StandingOrdersCommand {
    /// List standing orders
    ///
    /// List the standing orders stored in the vault, ordered by account: id,
    /// account, payee, purpose, the amount with its currency (negative is money
    /// leaving the account), the interval, the day it runs on, the next
    /// execution day and the state.
    ///
    /// The day is the day of the month, or of the week for a weekly order; 97 to
    /// 99 stand for days counted from the end of the month. The next execution
    /// day is known only for orders that are kept in Banking4 (state `local`).
    /// For an order held at the bank (state `at_bank`) Banking4 computes it when
    /// it shows the order, and b4 does not: the field is empty.
    ///
    /// With --output json, jsonl or csv every standing order has these fields,
    /// named as in `b4 schema standing_orders`: account_id, id, account_name,
    /// scheme, amount, currency, debit, payee_name, payee_iban, payee_bic,
    /// purpose, category, state, is_local, period_unit, period_length,
    /// execution_day, first_execution_date, last_execution_date,
    /// next_execution_date, stored_next_execution_date.
    List(StandingOrdersListArgs),
}

/// Arguments of `b4 standing-orders list`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 standing-orders list
      List the standing orders of all accounts.
  b4 standing-orders list --account giro
      Only those of one account.")]
pub struct StandingOrdersListArgs {
    /// Only standing orders of this account
    #[arg(
        long,
        value_name = "ACCOUNT",
        value_parser = NonEmptyStringValueParser::new(),
        long_help = concat!("Only standing orders of this account\n\n", account_rule!())
    )]
    pub account: Option<String>,
}

/// The verbs of `b4 portfolio`.
#[derive(Debug, Clone, Subcommand)]
pub enum PortfolioCommand {
    /// List securities positions
    ///
    /// List the positions of the securities accounts, ordered by account and
    /// name: account, ISIN, name, quantity, the latest price with its currency
    /// and day, the purchase price with its currency and day, and the state.
    ///
    /// Only what the vault stores is shown. b4 computes no market value and no
    /// gain: a price can be quoted per unit or in percent of a face amount and in
    /// another currency than the account's, and the vault does not hold what it
    /// takes to turn that into an amount. `b4 sql` on the tables portfolio_items
    /// and stock_prices is the place for such sums.
    ///
    /// With --output json, jsonl or csv every position has these fields, named as
    /// in `b4 schema portfolio_items`: account_id, id, account_name, isin, name,
    /// quantity, quantity_type, quantity_currency, price_type, current_price,
    /// current_price_currency, current_price_date, purchase_price,
    /// purchase_price_currency, purchase_price_date, state, deleted. Quantities
    /// and prices are exact there; the table shows the quantity with as many
    /// decimals as the listed positions need.
    List(PortfolioListArgs),
}

/// Arguments of `b4 portfolio list`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 portfolio list
      List the positions of all securities accounts.
  b4 portfolio list --account depot --output json
      The positions of one account, as JSON.")]
pub struct PortfolioListArgs {
    /// Only positions of this account
    #[arg(
        long,
        value_name = "ACCOUNT",
        value_parser = NonEmptyStringValueParser::new(),
        long_help = concat!("Only positions of this account\n\n", account_rule!())
    )]
    pub account: Option<String>,
}

// --- sql, schema, export -----------------------------------------------------

/// Arguments of `b4 sql`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 sql 'select count(*) from bookings'
      Count the bookings.
  b4 sql 'select category, sum(amount) from bookings where counted group by 1'
      Net amount per category, every booking counted once.
  b4 sql - < report.sql
      Read the query from a file.
  b4 sql 'select * from accounts' --output jsonl
      Every column of every account, one JSON object per line.")]
pub struct SqlArgs {
    /// The SQL query, or `-` to read it from standard input
    #[arg(value_name = "QUERY", allow_hyphen_values = true, value_parser = NonEmptyStringValueParser::new())]
    pub query: String,
}

impl SqlArgs {
    /// Whether the query is to be read from standard input.
    pub fn reads_stdin(&self) -> bool {
        self.query == "-"
    }
}

/// Arguments of `b4 schema`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 schema
      List the tables.
  b4 schema bookings
      List the columns of the bookings table.")]
pub struct SchemaArgs {
    /// Show the columns of this table instead of the list of tables
    #[arg(value_name = "TABLE", value_parser = NonEmptyStringValueParser::new())]
    pub table: Option<String>,
}

/// Arguments of `b4 export`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 export ./export
      Write every table as CSV into ./export.
  b4 export ./export --format jsonl --table bookings --table accounts
      Write two tables as JSON lines.")]
pub struct ExportArgs {
    /// The directory to write the files into
    #[arg(value_name = "DIR", value_parser = parse_path, value_hint = ValueHint::DirPath)]
    pub dir: PathBuf,

    /// The file format [default: csv]
    ///
    /// Without --format, the global --output selects the format when it is
    /// csv or jsonl. `--output table` and `--output json` are errors, and so
    /// is an --output that contradicts --format: export writes csv and jsonl
    /// only.
    #[arg(long, value_enum, value_name = "FORMAT")]
    pub format: Option<ExportFormat>,

    /// Export only this table; repeat for several tables
    #[arg(long = "table", value_name = "TABLE", value_parser = NonEmptyStringValueParser::new())]
    pub tables: Vec<String>,
}

/// The file formats of `b4 export --format`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ExportFormat {
    /// Comma-separated values with a header line
    Csv,
    /// One JSON object per line
    Jsonl,
}

impl ExportFormat {
    /// The extension of the files, without the dot.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Csv => "csv",
            ExportFormat::Jsonl => "jsonl",
        }
    }
}

// --- completions -------------------------------------------------------------

/// Arguments of `b4 completions`.
#[derive(Debug, Clone, Args)]
#[command(after_long_help = "\
Examples:
  b4 completions bash > ~/.local/share/bash-completion/completions/b4
  b4 completions zsh > ~/.zfunc/_b4
  b4 completions fish > ~/.config/fish/completions/b4.fish")]
pub struct CompletionsArgs {
    /// The shell to print the script for
    #[arg(value_enum, value_name = "SHELL")]
    pub shell: Shell,
}

/// The shells `b4 completions` knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    /// GNU Bash
    Bash,
    /// Z shell
    Zsh,
    /// fish
    Fish,
}

// --- value parsers -----------------------------------------------------------

/// A path that is not empty. clap hands over `--db ''` and an empty `B4_DB`
/// as they are, and an empty path only fails later with a puzzling message.
fn parse_path(text: &str) -> Result<PathBuf, String> {
    if text.is_empty() {
        return Err("the path is empty".to_owned());
    }
    Ok(PathBuf::from(text))
}

/// A DATE argument: a day, a month or a year.
///
/// Which day a month or a year stands for depends on the option it is given
/// to, so the precision is kept: [`first_day`](DateArg::first_day) for
/// `--from`, [`last_day`](DateArg::last_day) for `--to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateArg {
    /// `YYYY-MM-DD`
    Day(Date),
    /// `YYYY-MM`
    Month {
        /// The year.
        year: i32,
        /// The month, 1 to 12.
        month: u8,
    },
    /// `YYYY`
    Year(i32),
}

impl DateArg {
    /// The first day this argument covers.
    pub fn first_day(self) -> Date {
        let (year, month, day) = match self {
            DateArg::Day(date) => return date,
            DateArg::Month { year, month } => (year, month, 1),
            DateArg::Year(year) => (year, 1, 1),
        };
        // The parser only builds months 1 to 12, whose first day exists.
        Date::new(year, month, day).unwrap_or(Date::from_days_since_epoch(0))
    }

    /// The last day this argument covers.
    pub fn last_day(self) -> Date {
        let (year, month) = match self {
            DateArg::Day(date) => return date,
            DateArg::Month { year, month } => (year, month),
            DateArg::Year(year) => (year, 12),
        };
        Date::new(year, month, days_in_month(year, month)).unwrap_or(self.first_day())
    }
}

impl FromStr for DateArg {
    type Err = String;

    fn from_str(text: &str) -> Result<DateArg, String> {
        const EXPECTED: &str = "expected YYYY-MM-DD, YYYY-MM or YYYY";

        let mut parts = text.split('-');
        let year = parts
            .next()
            .and_then(|part| digits(part, 4))
            .ok_or(EXPECTED)?;
        let month = parts
            .next()
            .map(|part| digits(part, 2).ok_or(EXPECTED))
            .transpose()?;
        let day = parts
            .next()
            .map(|part| digits(part, 2).ok_or(EXPECTED))
            .transpose()?;
        if parts.next().is_some() {
            return Err(EXPECTED.to_owned());
        }

        // The widths above keep all three far inside these types.
        let year = year as i32;
        if year == 0 {
            return Err("there is no year 0000".to_owned());
        }
        let Some(month) = month.map(|month| month as u8) else {
            return Ok(DateArg::Year(year));
        };
        if !(1..=12).contains(&month) {
            return Err(format!("there is no month {month:02}"));
        }
        let Some(day) = day.map(|day| day as u8) else {
            return Ok(DateArg::Month { year, month });
        };
        Date::new(year, month, day)
            .map(DateArg::Day)
            .ok_or_else(|| format!("{year:04}-{month:02} has no day {day:02}"))
    }
}

/// The value of `text` if it consists of exactly `width` ASCII digits.
fn digits(text: &str, width: usize) -> Option<u32> {
    if text.len() != width || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// Parse the N of `--limit`: a whole number of at least 1. A limit of nothing
/// would print nothing, which hides the typing error.
fn parse_limit(text: &str) -> Result<u64, String> {
    match text.parse::<u64>() {
        Ok(limit) if limit > 0 => Ok(limit),
        _ => Err("expected a whole number of at least 1".to_owned()),
    }
}

/// An AMOUNT argument: digits with an optional decimal part after a point or
/// a comma, parsed exactly.
///
/// People write amounts in two ways, `1,234.56` and `1.234,56`, and the two
/// conventions disagree about what `1.234` means. Guessing wrong by a factor
/// of a thousand is worse than asking again, so thousands separators are not
/// accepted at all, and a single separator followed by exactly three digits,
/// which may be either, is rejected as ambiguous. There is no sign: the
/// filters that take an amount compare the absolute value.
fn parse_amount(text: &str) -> Result<Decimal, String> {
    const EXPECTED: &str = "expected a number like 1234.56 or 1234,56";

    if text.starts_with(['-', '+']) {
        return Err(
            "write the amount without a sign; it is compared with the absolute \
                    amount of a booking (use --debit or --credit for the direction)"
                .to_owned(),
        );
    }
    // Checked here and not left to the decimal parser, which is more liberal
    // (it accepts `_` between digits, for one).
    if text.is_empty()
        || !text
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'.' | b','))
    {
        return Err(EXPECTED.to_owned());
    }

    let mut parts = text.split(['.', ',']);
    let whole = parts.next().unwrap_or("");
    let fraction = parts.next();
    if parts.next().is_some() {
        return Err("thousands separators are not accepted; write 1234567.89".to_owned());
    }
    if whole.is_empty() || fraction == Some("") {
        return Err(EXPECTED.to_owned());
    }
    if let Some(fraction) = fraction {
        let looks_grouped = fraction.len() == 3 && whole.len() <= 3 && !whole.starts_with('0');
        if looks_grouped {
            return Err(format!(
                "ambiguous: write {whole}{fraction} for the whole number or \
                 {whole}.{fraction}0 for the decimal"
            ));
        }
    }

    let normalised = match fraction {
        Some(fraction) => format!("{whole}.{fraction}"),
        None => whole.to_owned(),
    };
    Decimal::from_str_exact(&normalised).map_err(|_| "the number has too many digits".to_owned())
}

#[cfg(test)]
mod tests {
    use clap::error::ErrorKind;

    use super::*;

    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("b4").chain(args.iter().copied()))
    }

    fn list_filters(args: &[&str]) -> BookingFilters {
        let mut all = vec!["bookings", "list"];
        all.extend_from_slice(args);
        match parse(&all).unwrap().command {
            Command::Bookings(BookingsCommand::List(list)) => list.filters,
            other => panic!("unexpected command {other:?}"),
        }
    }

    fn date(text: &str) -> Date {
        match text.parse().unwrap() {
            DateArg::Day(date) => date,
            other => panic!("{text} is not a day: {other:?}"),
        }
    }

    /// Every command of the tree, the root included. clap's own `help`
    /// command mirrors the tree and is left out.
    fn all_commands(command: &clap::Command, found: &mut Vec<clap::Command>) {
        found.push(command.clone());
        for subcommand in command.get_subcommands().filter(|c| c.get_name() != "help") {
            all_commands(subcommand, found);
        }
    }

    fn command_tree() -> Vec<clap::Command> {
        let mut root = Cli::command();
        root.build();
        let mut found = Vec::new();
        all_commands(&root, &mut found);
        found
    }

    #[test]
    fn the_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn no_command_has_a_password_option() {
        let commands = command_tree();
        assert!(
            commands.len() > 15,
            "the walk found only {} commands",
            commands.len()
        );

        let mut password_options = Vec::new();
        for command in &commands {
            for arg in command.get_arguments() {
                let mut names: Vec<String> = Vec::new();
                names.extend(arg.get_long().map(str::to_owned));
                names.extend(
                    arg.get_all_aliases()
                        .unwrap_or_default()
                        .iter()
                        .map(|a| a.to_string()),
                );
                names.push(arg.get_id().to_string());
                for name in names {
                    let name = name.to_lowercase().replace('_', "-");
                    if name.contains("pass") || name.contains("pw") || name.contains("secret") {
                        password_options.push((command.get_name().to_owned(), name));
                    }
                }
                // No option may read the password from an environment
                // variable through clap either: clap prints such values in
                // --help.
                assert_ne!(
                    arg.get_env().and_then(|env| env.to_str()),
                    Some("B4_PASSWORD")
                );
            }
        }
        password_options.sort();
        password_options.dedup();

        // The one exception takes no value: it is a switch.
        assert_eq!(
            password_options,
            vec![("decrypt".to_owned(), "password-stdin".to_owned())]
        );
        let decrypt = Cli::command();
        let decrypt = decrypt.find_subcommand("decrypt").unwrap();
        let switch = decrypt
            .get_arguments()
            .find(|a| a.get_id() == "password_stdin")
            .unwrap();
        assert!(!switch.get_action().takes_values());

        // clap does not complete `--password` to `--password-stdin`.
        for args in [
            &["decrypt", "vault.sub", "--password", "x"][..],
            &["decrypt", "vault.sub", "--password=x"],
            &["decrypt", "vault.sub", "--password"],
            &["--password", "x", "decrypt", "vault.sub"],
            &["decrypt", "vault.sub", "-p", "x"],
        ] {
            assert_eq!(
                parse(args).unwrap_err().kind(),
                ErrorKind::UnknownArgument,
                "{args:?}"
            );
        }
    }

    #[test]
    fn every_command_has_help_and_leaves_have_examples() {
        for command in command_tree() {
            let name = command.get_name().to_owned();
            assert!(command.get_about().is_some(), "{name} has no description");
            if command.get_subcommands().next().is_none() {
                let examples = command.get_after_long_help().map(ToString::to_string);
                let examples = examples.unwrap_or_default();
                assert!(
                    examples.starts_with("Examples:\n  b4 "),
                    "{name} has no examples"
                );
                for line in examples.lines() {
                    assert!(
                        line.chars().count() <= 80,
                        "{name}: example line too long: {line}"
                    );
                }
            }
        }
    }

    #[test]
    fn global_options_have_defaults() {
        let cli = parse(&["status"]).unwrap();
        // B4_DB may be set where the tests run; only check without it.
        if std::env::var_os("B4_DB").is_none() {
            assert_eq!(cli.globals.db, PathBuf::from("./b4.duckdb"));
        }
        assert_eq!(cli.globals.output, None);
        assert_eq!(cli.globals.format(), Format::Table);
        assert!(!cli.globals.no_header);
        assert!(!cli.globals.quiet);
    }

    #[test]
    fn global_options_work_before_and_after_the_command() {
        let before = parse(&["--db", "x.duckdb", "-o", "json", "-q", "accounts", "list"]).unwrap();
        let after = parse(&[
            "accounts", "list", "--db", "x.duckdb", "--output", "json", "--quiet",
        ])
        .unwrap();
        for cli in [before, after] {
            assert_eq!(cli.globals.db, PathBuf::from("x.duckdb"));
            assert_eq!(cli.globals.output, Some(Format::Json));
            assert!(cli.globals.quiet);
        }
        let cli = parse(&["bookings", "--no-header", "list", "-o", "csv"]).unwrap();
        assert!(cli.globals.no_header);
        assert_eq!(cli.globals.format(), Format::Csv);
    }

    #[test]
    fn output_formats() {
        for (name, format) in [
            ("table", Format::Table),
            ("json", Format::Json),
            ("jsonl", Format::Jsonl),
            ("csv", Format::Csv),
        ] {
            assert_eq!(
                parse(&["status", "-o", name]).unwrap().globals.format(),
                format
            );
        }
        let error = parse(&["status", "-o", "xml"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn an_empty_path_is_rejected() {
        assert_eq!(
            parse(&["--db", "", "status"]).unwrap_err().kind(),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            parse(&["info", ""]).unwrap_err().kind(),
            ErrorKind::ValueValidation
        );
    }

    #[test]
    fn a_command_is_required() {
        let error = parse(&[]).unwrap_err();
        // With B4_DB set where the tests run, clap sees an argument and names
        // the missing command instead of printing the help.
        assert!(
            matches!(
                error.kind(),
                ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand | ErrorKind::MissingSubcommand
            ),
            "{:?}",
            error.kind()
        );
        assert_eq!(error.exit_code(), 2);
        let error = parse(&["bookings"]).unwrap_err();
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn decrypt_writes_to_out_or_to_the_database() {
        let cli = parse(&["decrypt", "vault.sub", "--db", "global.duckdb"]).unwrap();
        let Command::Decrypt(args) = &cli.command else {
            panic!()
        };
        assert_eq!(args.vault, PathBuf::from("vault.sub"));
        assert!(!args.password_stdin);
        assert_eq!(args.target(&cli.globals), Path::new("global.duckdb"));

        let cli = parse(&[
            "decrypt",
            "vault.sub",
            "out.duckdb",
            "--db",
            "global.duckdb",
            "--password-stdin",
        ])
        .unwrap();
        let Command::Decrypt(args) = &cli.command else {
            panic!()
        };
        assert!(args.password_stdin);
        assert_eq!(args.target(&cli.globals), Path::new("out.duckdb"));

        assert_eq!(
            parse(&["decrypt"]).unwrap_err().kind(),
            ErrorKind::MissingRequiredArgument
        );
    }

    #[test]
    fn accounts_list_takes_several_types() {
        let cli = parse(&[
            "accounts",
            "list",
            "--type",
            "bank",
            "--type",
            "credit-card",
            "--all",
        ])
        .unwrap();
        let Command::Accounts(AccountsCommand::List(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.types, vec![AccountType::Bank, AccountType::CreditCard]);
        assert!(args.all);
        assert_eq!(AccountType::CreditCard.kind_name(), "credit_card");

        let error = parse(&["accounts", "list", "--type", "savings"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::InvalidValue);
    }

    #[test]
    fn every_account_type_has_a_kind_name_in_snake_case() {
        for account_type in AccountType::value_variants() {
            let on_the_command_line = account_type.to_possible_value().unwrap();
            assert_eq!(
                account_type.kind_name(),
                on_the_command_line.get_name().replace('-', "_")
            );
        }
    }

    #[test]
    fn bookings_list_defaults() {
        let cli = parse(&["bookings", "list"]).unwrap();
        let Command::Bookings(BookingsCommand::List(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.sort, BookingSort::Date);
        assert!(!args.reverse);
        assert_eq!(args.limit, None);
        assert!(args.filters.accounts.is_empty());
        assert_eq!(args.filters.first_day(), None);
        assert_eq!(args.filters.last_day(), None);
    }

    #[test]
    fn bookings_list_takes_every_filter() {
        let cli = parse(&[
            "bookings",
            "list",
            "--account",
            "giro",
            "--account",
            "DE89 3704",
            "--from",
            "2026-02",
            "--to",
            "2026",
            "--filter",
            "rewe",
            "--payee",
            "markt",
            "--category",
            "food",
            "--min",
            "10",
            "--max",
            "99,90",
            "--debit",
            "--booked",
            "--sort",
            "amount",
            "--reverse",
            "--limit",
            "20",
        ])
        .unwrap();
        let Command::Bookings(BookingsCommand::List(args)) = cli.command else {
            panic!()
        };
        let filters = &args.filters;
        assert_eq!(filters.accounts, vec!["giro", "DE89 3704"]);
        assert_eq!(filters.first_day(), Date::new(2026, 2, 1));
        assert_eq!(filters.last_day(), Date::new(2026, 12, 31));
        assert_eq!(filters.filter.as_deref(), Some("rewe"));
        assert_eq!(filters.payee.as_deref(), Some("markt"));
        assert_eq!(filters.category.as_deref(), Some("food"));
        assert_eq!(filters.min, Some(Decimal::new(10, 0)));
        assert_eq!(filters.max, Some(Decimal::new(9990, 2)));
        assert!(filters.debit && filters.booked);
        assert!(!filters.credit && !filters.pending && !filters.uncategorized);
        assert_eq!(args.sort, BookingSort::Amount);
        assert!(args.reverse);
        assert_eq!(args.limit, Some(20));
    }

    #[test]
    fn opposite_filters_conflict() {
        for args in [
            &["bookings", "list", "--debit", "--credit"][..],
            &["bookings", "list", "--credit", "--debit"],
            &["bookings", "list", "--pending", "--booked"],
            &["bookings", "list", "--booked", "--pending"],
            &["bookings", "list", "--uncategorized", "--category", "x"],
            &[
                "bookings", "summary", "--by", "month", "--debit", "--credit",
            ],
            &[
                "bookings",
                "summary",
                "--by",
                "month",
                "--pending",
                "--booked",
            ],
        ] {
            assert_eq!(
                parse(args).unwrap_err().kind(),
                ErrorKind::ArgumentConflict,
                "{args:?}"
            );
        }
        // Filters of different kinds combine.
        assert!(parse(&[
            "bookings",
            "list",
            "--debit",
            "--pending",
            "--uncategorized"
        ])
        .is_ok());
    }

    #[test]
    fn empty_filter_texts_and_bad_limits_are_rejected() {
        for args in [
            &["bookings", "list", "--filter", ""][..],
            &["bookings", "list", "--payee", ""],
            &["bookings", "list", "--account", ""],
            &["bookings", "list", "--limit", "0"],
            &["bookings", "list", "--limit", "-1"],
            &["bookings", "list", "--limit", "many"],
            &["bookings", "list", "--sort", "iban"],
            &["sql", ""],
        ] {
            assert!(parse(args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn reversed_ranges_fail_validation() {
        let cli = parse(&["bookings", "list", "--from", "2026-05", "--to", "2026-04"]).unwrap();
        assert_eq!(
            cli.validate().unwrap_err(),
            "--from (2026-05-01) is after --to (2026-04-30)"
        );

        let cli = parse(&[
            "bookings", "summary", "--by", "year", "--min", "10", "--max", "9,99",
        ])
        .unwrap();
        assert_eq!(
            cli.validate().unwrap_err(),
            "--min (10) is greater than --max (9.99)"
        );

        // One month, one day, equal bounds: all fine.
        for args in [
            &["bookings", "list", "--from", "2026-05", "--to", "2026-05"][..],
            &[
                "bookings",
                "list",
                "--from",
                "2026-05-17",
                "--to",
                "2026-05-17",
            ],
            &["bookings", "list", "--min", "5", "--max", "5.00"],
            &["bookings", "list", "--from", "2026"],
            &["accounts", "list"],
        ] {
            assert_eq!(parse(args).unwrap().validate(), Ok(()), "{args:?}");
        }
    }

    #[test]
    fn bookings_show_takes_an_id() {
        let cli = parse(&["bookings", "show", "815522031"]).unwrap();
        let Command::Bookings(BookingsCommand::Show(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.id, 815_522_031);
        // Ids are signed in the vault.
        let cli = parse(&["bookings", "show", "-5"]).unwrap();
        let Command::Bookings(BookingsCommand::Show(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.id, -5);
        assert!(parse(&["bookings", "show", "abc"]).is_err());
        assert!(parse(&["bookings", "show"]).is_err());
    }

    #[test]
    fn bookings_summary_requires_a_grouping() {
        let error = parse(&["bookings", "summary"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);

        for (name, by) in [
            ("month", SummaryBy::Month),
            ("year", SummaryBy::Year),
            ("category", SummaryBy::Category),
            ("payee", SummaryBy::Payee),
            ("account", SummaryBy::Account),
        ] {
            let cli = parse(&["bookings", "summary", "--by", name, "--from", "2026"]).unwrap();
            let Command::Bookings(BookingsCommand::Summary(args)) = cli.command else {
                panic!()
            };
            assert_eq!(args.by, by);
            assert_eq!(args.filters.first_day(), Date::new(2026, 1, 1));
        }
    }

    #[test]
    fn the_remaining_commands_parse() {
        let cli = parse(&["payees", "list", "--filter", "insurance"]).unwrap();
        let Command::Payees(PayeesCommand::List(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.filter.as_deref(), Some("insurance"));

        assert!(matches!(
            parse(&["categories", "list"]).unwrap().command,
            Command::Categories(CategoriesCommand::List(_))
        ));

        let cli = parse(&["standing-orders", "list", "--account", "giro"]).unwrap();
        let Command::StandingOrders(StandingOrdersCommand::List(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.account.as_deref(), Some("giro"));

        let cli = parse(&["portfolio", "list"]).unwrap();
        let Command::Portfolio(PortfolioCommand::List(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.account, None);

        let cli = parse(&["status", "--check"]).unwrap();
        let Command::Status(args) = cli.command else {
            panic!()
        };
        assert!(args.check);

        let cli = parse(&["schema"]).unwrap();
        let Command::Schema(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.table, None);
        let cli = parse(&["schema", "bookings"]).unwrap();
        let Command::Schema(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.table.as_deref(), Some("bookings"));

        let cli = parse(&["accounts", "show", "DE89 3704 0044 0532 0130 00"]).unwrap();
        let Command::Accounts(AccountsCommand::Show(args)) = cli.command else {
            panic!()
        };
        assert_eq!(args.account, "DE89 3704 0044 0532 0130 00");
    }

    #[test]
    fn sql_takes_a_query_or_a_dash() {
        let cli = parse(&["sql", "select 1"]).unwrap();
        let Command::Sql(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.query, "select 1");
        assert!(!args.reads_stdin());

        let cli = parse(&["sql", "-"]).unwrap();
        let Command::Sql(args) = cli.command else {
            panic!()
        };
        assert!(args.reads_stdin());
    }

    #[test]
    fn export_defaults_to_csv_of_every_table() {
        let cli = parse(&["export", "out"]).unwrap();
        let Command::Export(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.dir, PathBuf::from("out"));
        // Not given: the command decides, from --output or csv.
        assert_eq!(args.format, None);
        assert!(args.tables.is_empty());
        assert_eq!(ExportFormat::Csv.extension(), "csv");

        let cli = parse(&[
            "export", "out", "--format", "jsonl", "--table", "bookings", "--table", "accounts",
        ])
        .unwrap();
        let Command::Export(args) = cli.command else {
            panic!()
        };
        assert_eq!(args.format, Some(ExportFormat::Jsonl));
        assert_eq!(args.tables, vec!["bookings", "accounts"]);
        assert_eq!(ExportFormat::Jsonl.extension(), "jsonl");

        // json is not a file format of export. The global --output is
        // accepted by the parser and judged by the command.
        assert!(parse(&["export", "out", "--format", "json"]).is_err());
        let cli = parse(&["export", "out", "--output", "jsonl"]).unwrap();
        assert_eq!(cli.globals.output, Some(Format::Jsonl));
        // A table that is asked for is not the same as no --output: export
        // refuses the first, wherever the option stands.
        for args in [
            ["export", "out", "--output", "table"],
            ["--output", "table", "export", "out"],
        ] {
            assert_eq!(parse(&args).unwrap().globals.output, Some(Format::Table));
        }
        assert_eq!(parse(&["export", "out"]).unwrap().globals.output, None);
    }

    #[test]
    fn completions_knows_three_shells() {
        for (name, shell) in [
            ("bash", Shell::Bash),
            ("zsh", Shell::Zsh),
            ("fish", Shell::Fish),
        ] {
            let cli = parse(&["completions", name]).unwrap();
            let Command::Completions(args) = cli.command else {
                panic!()
            };
            assert_eq!(args.shell, shell);
        }
        assert!(parse(&["completions", "powershell"]).is_err());
        assert!(parse(&["completions"]).is_err());
    }

    // --- DATE ---

    #[test]
    fn date_as_a_day() {
        assert_eq!(date("2026-03-14"), Date::new(2026, 3, 14).unwrap());
        assert_eq!(date("2024-02-29"), Date::new(2024, 2, 29).unwrap());
        let day: DateArg = "2026-03-14".parse().unwrap();
        assert_eq!(day.first_day(), day.last_day());
    }

    #[test]
    fn date_as_a_month_covers_the_whole_month() {
        let month: DateArg = "2026-02".parse().unwrap();
        assert_eq!(
            month,
            DateArg::Month {
                year: 2026,
                month: 2
            }
        );
        assert_eq!(month.first_day().to_string(), "2026-02-01");
        assert_eq!(month.last_day().to_string(), "2026-02-28");

        let leap: DateArg = "2024-02".parse().unwrap();
        assert_eq!(leap.last_day().to_string(), "2024-02-29");
        let december: DateArg = "2026-12".parse().unwrap();
        assert_eq!(december.last_day().to_string(), "2026-12-31");
        let april: DateArg = "2026-04".parse().unwrap();
        assert_eq!(april.last_day().to_string(), "2026-04-30");
    }

    #[test]
    fn date_as_a_year_covers_the_whole_year() {
        let year: DateArg = "2026".parse().unwrap();
        assert_eq!(year, DateArg::Year(2026));
        assert_eq!(year.first_day().to_string(), "2026-01-01");
        assert_eq!(year.last_day().to_string(), "2026-12-31");
    }

    #[test]
    fn date_rejects_everything_else() {
        for text in [
            "",
            "26",
            "202",
            "20260",
            "2026-3",
            "2026-03-1",
            "2026-003",
            "2026/03/14",
            "14.03.2026",
            "2026-03-14-01",
            "2026-",
            "2026-03-",
            "-2026",
            "2026-00",
            "2026-13",
            "2026-02-30",
            "2023-02-29",
            "2026-04-31",
            "2026-01-00",
            "0000",
            "0000-01-01",
            "2026-1x",
            "２０２６",
            " 2026",
            "2026 ",
            "+026",
            "today",
        ] {
            assert!(text.parse::<DateArg>().is_err(), "{text:?} was accepted");
        }
    }

    #[test]
    fn date_errors_say_what_is_wrong() {
        let error = |text: &str| text.parse::<DateArg>().unwrap_err();
        assert_eq!(error("14.03.2026"), "expected YYYY-MM-DD, YYYY-MM or YYYY");
        assert_eq!(error("2026-13"), "there is no month 13");
        assert_eq!(error("2026-02-30"), "2026-02 has no day 30");
        assert_eq!(error("0000"), "there is no year 0000");
    }

    #[test]
    fn from_and_to_read_the_same_text_differently() {
        let filters = list_filters(&["--from", "2026-03", "--to", "2026-03"]);
        assert_eq!(filters.first_day(), Date::new(2026, 3, 1));
        assert_eq!(filters.last_day(), Date::new(2026, 3, 31));

        let filters = list_filters(&["--from", "2025", "--to", "2025"]);
        assert_eq!(filters.first_day(), Date::new(2025, 1, 1));
        assert_eq!(filters.last_day(), Date::new(2025, 12, 31));

        assert!(parse(&["bookings", "list", "--from", "yesterday"]).is_err());
    }

    // --- AMOUNT ---

    #[test]
    fn amount_accepts_a_point_or_a_comma() {
        let amount = |text: &str| parse_amount(text).unwrap();
        assert_eq!(amount("1234.56"), Decimal::new(123_456, 2));
        assert_eq!(amount("1234,56"), Decimal::new(123_456, 2));
        assert_eq!(amount("100"), Decimal::new(100, 0));
        assert_eq!(amount("0"), Decimal::ZERO);
        assert_eq!(amount("0,5"), Decimal::new(5, 1));
        assert_eq!(amount("19.9"), Decimal::new(199, 1));
        assert_eq!(amount("007"), Decimal::new(7, 0));
        // Three decimals are a decimal when the number cannot be grouped.
        assert_eq!(amount("0.125"), Decimal::new(125, 3));
        assert_eq!(amount("1234.567"), Decimal::new(1_234_567, 3));
        assert_eq!(amount("1.2345"), Decimal::new(12_345, 4));
    }

    #[test]
    fn amount_is_exact() {
        // 0.1 + 0.2 is the classic float failure.
        let sum = parse_amount("0.1").unwrap() + parse_amount("0.2").unwrap();
        assert_eq!(sum, parse_amount("0.3").unwrap());
        assert_eq!(
            parse_amount("1234567890123456.789012").unwrap().to_string(),
            "1234567890123456.789012"
        );
    }

    #[test]
    fn amount_rejects_ambiguous_separators() {
        for text in ["1.234", "1,234", "12.345", "123,456", "999.000"] {
            let error = parse_amount(text).unwrap_err();
            assert!(error.starts_with("ambiguous"), "{text}: {error}");
        }
        assert_eq!(
            parse_amount("1.234").unwrap_err(),
            "ambiguous: write 1234 for the whole number or 1.2340 for the decimal"
        );
        for text in [
            "1.234,56",
            "1,234.56",
            "1.234.567",
            "1,234,567",
            "1..2",
            "1.,2",
        ] {
            let error = parse_amount(text).unwrap_err();
            assert!(error.contains("thousands separators"), "{text}: {error}");
        }
    }

    #[test]
    fn amount_rejects_everything_else() {
        for text in [
            "", " ", "abc", "12a", "1 234", "1'234", "1_234", "12.", ".5", ",5", ".", "1e3",
            "12 EUR", "€12", "0x10", "١٢", " 12", "12 ",
        ] {
            assert!(parse_amount(text).is_err(), "{text:?} was accepted");
        }
        // More digits than an exact decimal holds.
        assert!(parse_amount(&"9".repeat(40)).is_err());
        assert!(parse_amount(&format!("0.{}", "1".repeat(40))).is_err());
    }

    #[test]
    fn amount_has_no_sign() {
        for text in ["-5", "+5", "-0.50"] {
            let error = parse_amount(text).unwrap_err();
            assert!(error.contains("without a sign"), "{text}: {error}");
        }
        let error = parse(&["bookings", "list", "--min=-5"]).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::ValueValidation);
    }
}
