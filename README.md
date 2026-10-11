# b4

```text
    __    __ __
   / /_  / // /      .-------------.           .--------------.
  / __ \/ // /_      | .sub  [###] |  decrypt  | select *     |
 / /_/ /__  __/      | vault  (o)  | --------> | from money;  |
/_.___/  /_/         '-------------'           '--------------'
```

*Query your money b4 it's gone.*

A command-line reader for [Banking4](https://subsembly.com/banking4.html) data
vaults (`.sub` files). `b4 decrypt` turns a vault into a
[DuckDB](https://duckdb.org/) database; every other command queries that
database. Scripts and AI agents can then search and export accounts, bookings,
payees and categories without the Banking4 GUI and without ever seeing the
vault password.

```console
$ b4 decrypt ~/Banking/Haushaltsbuch.sub     # asks for the vault password, writes ./b4.duckdb
$ b4 accounts list
$ b4 bookings list --account Girokonto --from 2025-01 --filter miete
$ b4 bookings summary --by category --from 2025
$ b4 sql "select category, sum(amount) from bookings where counted group by 1 order by 2"
```

> **Unofficial.** This is an independent tool. It is not affiliated with or
> endorsed by Subsembly GmbH, the maker of Banking4. It only ever reads a vault
> and never writes to it. Use it on your own data and at your own risk.

## Why

Banking4 keeps everything in one encrypted vault file and offers no way to get
at the data from a script in its Home edition. `b4` reads the vault format
natively: it needs no Banking4 installation, no Windows and no Wine. The
result is an ordinary DuckDB file that you can query with `b4`, with the
`duckdb` shell, or from Python, R and anything else that speaks DuckDB.

## Features

- **Native reader.** Decrypts the vault container and decodes the records
  itself, in safe Rust (`unsafe` is forbidden in the crate). A 15 MB vault with
  22,000 bookings decrypts in well under a second.
- **Decrypt once, query freely.** The password is needed for `b4 decrypt` only.
  Delete the database file and the data is locked away again.
- **Asks where you are.** On a terminal `b4 decrypt` prompts without echo;
  started without one, it asks in a dialog through an askpass program, so an
  agent can refresh the database without the password passing through it.
- **Exact money.** Amounts are `DECIMAL` columns, never floating point, so sums
  come out to the cent.
- **Made for scripts and agents.** `--output table|json|jsonl|csv`, stable
  column names, descriptions for every table and column (`b4 schema`), and a
  read-only, sandboxed `b4 sql`.
- **Leaves bank access data alone.** The parts of a vault that hold online
  banking credentials are never read (see [What b4 reads](#what-b4-reads-and-what-it-never-reads)).
- **Read-only on the vault.** The vault is opened for reading, copied into
  memory in one pass and closed. Nothing is written, locked or created next to
  it.
- Man pages and shell completions for bash, zsh and fish.

## Install

### With Nix (flakes)

```console
$ nix run github:ckesselh/b4 -- --help
$ nix profile install github:ckesselh/b4
```

In a flake-based NixOS or Home Manager configuration, add the input and its
overlay, then use `pkgs.b4`:

```nix
{
  inputs.b4 = {
    url = "github:ckesselh/b4";
    inputs.nixpkgs.follows = "nixpkgs";
  };

  # in your nixpkgs configuration:
  #   overlays = [ inputs.b4.overlays.default ];
  # then:
  #   home.packages = [ pkgs.b4 ];
}
```

The package installs the binary, the man pages and the shell completions.

### Prebuilt binaries

Every [release](https://github.com/ckesselh/b4/releases) has archives for
Linux and macOS, each for x86_64 and aarch64, with the binary, the man pages
and the shell completions:

```console
$ tar -xzf b4-0.1.0-x86_64-linux.tar.gz
$ sha256sum --check --ignore-missing SHA256SUMS
$ install -m 755 b4-0.1.0-x86_64-linux/b4 ~/.local/bin/
```

The Linux binaries need glibc 2.35 or newer (Debian 12, Ubuntu 22.04 and
later). On NixOS use the flake.

### From source

You need a Rust toolchain (1.85.1 or newer) and a C++ compiler; DuckDB is
compiled into the binary, so the first build takes a few minutes.

```console
$ git clone https://github.com/ckesselh/b4 && cd b4
$ cargo build --release
$ install -m 755 target/release/b4 ~/.local/bin/
```

## Quick start

```console
$ cd ~/finance
$ b4 decrypt ~/Banking/Haushaltsbuch.sub
Password:
accounts                      6
bookings                    634
categories                   30
payees                       30
…
bank access entries left unread: 9
database: ./b4.duckdb
try: b4 accounts list
$ b4 accounts list
$ b4 bookings list --from 2025-06 --limit 20
```

`b4 decrypt <VAULT> [OUT]` writes the database to `OUT`, or to `./b4.duckdb`
when no `OUT` is given. Running it again replaces the database with a fresh
one; that is also how you pick up new bookings after Banking4 has synchronised.
`b4 status` tells you whether the vault has changed since.

Every other command looks for the database in this order:

1. `--db <PATH>`
2. the environment variable `B4_DB`
3. `./b4.duckdb` in the current directory

To lock the data away again, delete the database file. There is nothing else to
clean up: `b4` keeps no configuration, no cache and no stored password.

## The vault password

`b4 decrypt` takes the password from, in this order:

1. `--password-stdin`: the first line of standard input,
   for example `pass show banking4 | b4 decrypt vault.sub --password-stdin`
2. the environment variable `B4_PASSWORD`
3. a prompt on the terminal, without echo
4. without a terminal: a program that asks for it, usually in a dialog

There is deliberately no `--password` option: command-line arguments are
visible to every process on the machine and end up in shell history. An
environment variable is visible to other processes of the same user, so prefer
stdin or the prompt. The password is never stored and is wiped from memory when
the command ends.

### Asking in a dialog when there is no terminal

The fourth source is for everything that starts `b4` without a terminal: a
desktop launcher, a timer, an AI agent. `b4 decrypt` then runs a program that
asks for the password, the way ssh and git do:

- the program named by the environment variable `B4_ASKPASS`, or else
- in a graphical session (`WAYLAND_DISPLAY` or `DISPLAY` is set), the program
  named by `SSH_ASKPASS`, unless `SSH_ASKPASS_REQUIRE` is `never`.

Many desktops set `SSH_ASKPASS` already; then there is nothing to set.
Otherwise, or to choose another program:

```console
$ export B4_ASKPASS=ksshaskpass                        # a name on PATH or a full path
$ setsid -w b4 decrypt ~/Banking/Haushaltsbuch.sub     # setsid starts b4 without a terminal: the dialog asks
```

Redirecting standard input is not enough to get the dialog: as long as `b4`
has a terminal, it asks there, and a prompt that you end with Ctrl-D is not
followed by a dialog.

The variable holds the name or path of one program, without arguments. `b4`
runs it with the question as its only argument (`b4: password for the Banking4
vault "Haushaltsbuch.sub": `) and without standard input, and takes the first
line it prints as the password. A status other than 0 means that the user
cancelled, and `b4 decrypt` fails without touching the database. That is the
convention of `ssh-askpass`, so `ksshaskpass`, `ssh-askpass`,
`lxqt-openssh-askpass` and the like fit as they are. A program with another
interface needs a two-line script around it:

```sh
#!/bin/sh
exec zenity --password --title="$1"
```

This way an AI agent can refresh the database without the password passing
through it: the agent runs `b4 decrypt`, the dialog appears on your screen,
and what you type goes to `b4` through a pipe. It does not appear in the
agent's transcript, in any argument or in the environment.

Know the limits of that:

- **It is a convenience, not a barrier.** Whoever starts `b4` chooses the
  program, because `B4_ASKPASS` can be set for a single command. An agent that
  wants your password can show you the same dialog and keep the answer, with
  or without `b4`. Use it with agents you would also let run `sudo`.
- **`b4` runs the program as it is** and cannot check it.
- **A password dialog you did not expect is a reason to cancel.** The question
  starts with `b4:` and names the vault file, but so can anything that
  imitates it.

## Usage

The examples use the demo vault of the test suite, an invented household
([tests/fixtures/vault](tests/fixtures/vault/README.md), password `demo`), so
you can reproduce every line:

```console
$ b4 decrypt tests/fixtures/vault/demo.sub
```

### Accounts

```console
$ b4 accounts list
       ID  TYPE         NAME         IBAN / NUMBER           BANK              BALANCE  AS OF       BOOKINGS
757073100  bank         Girokonto    DE31999900011000471100  MUBADEM0XXX  14733.44 EUR  2025-06-30       425
757073160  bank         Tagesgeld    DE09999900022000471122  BSPDDEB0XXX  18793.65 EUR  2025-06-30        26
757073220  credit_card  Kreditkarte  999900XXXXXX4711        99000000      -250.34 EUR  2025-06-25       123
757073280  portfolio    Depot        7000471133              99990002         0.00                         0
757073340  cash         Bargeld      757073340               00001000       391.74 EUR  2025-06-22        55
$ b4 accounts show giro          # everything about one account
$ b4 accounts list --all         # including accounts hidden in Banking4
```

Wherever a command takes an account, you can give its id, its IBAN, its
account number, or any part of its name that identifies it (`giro`).

### Bookings

```console
$ b4 bookings list --account giro --from 2025-06 --filter miete
       ID  DATE        ACCOUNT    PAYEE                         PURPOSE                                  CATEGORY           AMOUNT
802159201  2025-06-02  Girokonto  Hausverwaltung Beispiel GmbH  Miete 06/2025 Musterstraße 12, Whg. 4 …  Wohnen:Miete  -950.00 EUR
```

| Filter | Keeps bookings … |
|---|---|
| `--account <ACCOUNT>` | of that account (repeatable) |
| `--from <DATE>`, `--to <DATE>` | in that range, inclusive. `2025-06-15`, `2025-06` and `2025` all work: a month or year stands for its first day in `--from` and its last in `--to` |
| `--filter <TEXT>` | that contain the text in payee, purpose, booking text, category, notes or payee IBAN |
| `--payee <TEXT>` | whose payee name contains the text |
| `--category <TEXT>` | in that category or below it (`wohnen` matches `Wohnen:Miete`) |
| `--uncategorized` | without a category |
| `--min <AMOUNT>`, `--max <AMOUNT>` | by absolute amount |
| `--debit`, `--credit` | by direction |
| `--pending`, `--booked` | by status |

Text matches ignore case. `--sort date|amount|payee`, `--reverse` and
`--limit` shape the list; `--reverse --limit 20` gives the 20 most recent.
`b4 bookings show <ID>` prints every stored field of one booking.

```console
$ b4 bookings summary --by month --account giro --from 2025
MONTH    BOOKINGS        DEBITS      CREDITS          NET
2025-01        25  -2572.61 EUR  3342.50 EUR   769.89 EUR
2025-02        22  -2301.43 EUR  3403.11 EUR  1101.68 EUR
2025-03        28  -2941.99 EUR  4376.75 EUR  1434.76 EUR
2025-04        23  -2596.39 EUR  3342.50 EUR   746.11 EUR
2025-05        23  -2697.89 EUR  3342.50 EUR   644.61 EUR
2025-06        22  -2555.22 EUR  3342.50 EUR   787.28 EUR
```

`--by` takes `month`, `year`, `category`, `payee` or `account`, with the same
filters as `bookings list`. Sums are exact and never mix currencies.

### Categories, payees, standing orders, portfolio

```console
$ b4 categories list
CATEGORY                BOOKINGS            SUM  WITH CHILDREN  SUM WITH CHILDREN  NOTE
…
Wohnen                         1    -240.00 EUR             99      -21277.99 EUR
  Internet und Telefon        36    -989.64 EUR             36        -989.64 EUR
  Miete                       18  -17100.00 EUR             18      -17100.00 EUR
  Nebenkosten                  8    -332.35 EUR             44       -2948.35 EUR
    Heizung                   18   -1182.00 EUR             18       -1182.00 EUR
    Strom                     18   -1434.00 EUR             18       -1434.00 EUR
(uncategorized)               36   -1796.82 EUR             36       -1796.82 EUR
$ b4 payees list --filter stadtwerke
$ b4 standing-orders list
$ b4 portfolio list
```

### SQL

For everything the commands above do not cover:

```console
$ b4 sql "select category_top, sum(amount) as net
          from bookings_view
          where counted and not pending and booking_date >= date '2025-01-01'
          group by 1 order by 2 limit 5"
CATEGORY_TOP         NET
Wohnen        -7293.9100
Lebensmittel  -2714.6900
Mobilität     -1240.6800
Freizeit       -973.8600
Haushalt       -535.9400
$ b4 sql - < report.sql               # read the query from a file
$ b4 schema                           # which tables and views exist
$ b4 schema bookings                  # the columns of one, with descriptions
```

### Looking at a vault and at the database

```console
$ b4 info ~/Banking/Haushaltsbuch.sub     # header of a vault; needs no password
$ b4 status                               # what is in the database, warnings, is it current?
$ b4 status --check || b4 decrypt ~/Banking/Haushaltsbuch.sub --password-stdin < pw
$ b4 export out/ --format csv             # one file per table
```

Every command has `--help` and a man page (`man b4`, `man b4-bookings-list`).

### Output and exit codes

`-o, --output` selects the format for every query command:

| Format | What you get |
|---|---|
| `table` (default) | Aligned text for people. On a terminal, long text is cut with `…` to fit the width. |
| `json` | One array of objects with stable `snake_case` keys. |
| `jsonl` | One object per line. |
| `csv` | RFC 4180 with a header line (`--no-header` drops it). |

Numbers are exact in every format. JSON needs one rule, because many JSON
parsers read every number as a double-precision float: an integer is written
as a JSON number up to a magnitude of 2^53, a decimal when it has at most 15
significant digits (what a double carries through a round trip), and anything
beyond that as a string of the same digits.

Exit status: `0` success, `1` a runtime error (the message starts with
`error:` on stderr), `2` a usage error. `b4 status --check` exits `1` when the
database is out of date.

## The database

`b4 decrypt` writes one DuckDB file. Its tables use plain `snake_case` names,
and every table and column carries a description that `b4 schema` shows.

Things worth knowing before you write SQL:

- **Amounts are signed.** Negative means money leaving the account. The
  direction as stored by Banking4 is kept in a separate column.
- **`counted`.** A split or collective booking is stored as one master row
  plus its parts, and the two need not add up to the same amount. Use
  `where counted` in sums so nothing is counted twice; `b4 bookings summary`
  does.
- **Categories are text.** A booking names its category by full name
  (`Wohnen:Miete`); there are no category ids in a vault.
- **Everything the file holds is kept.** Records that Banking4 hides in its
  own lists are in the database too, marked with a boolean column.
- **No constraints.** Ids are unique within one account's file in practice,
  but the database does not enforce it; a repeated id is reported as a
  warning.
- **Warnings.** Anything the reader did not understand (an unknown entry in
  the vault, bytes a newer Banking4 version added to a record) is stored in the
  `warnings` table and shown by `b4 status`. They name files and counts, never
  values.

| Table | What it holds |
|---|---|
| `accounts`, `account_groups` | accounts of every kind with their stored balances, and the groups they are sorted into |
| `bookings` | statement entries, booked and pending |
| `payments`, `standing_orders` | payments entered in Banking4 and standing orders |
| `payees`, `payers`, `mandates` | the address book and direct debit mandates |
| `categories` | the category tree with budgets |
| `stock_instruments`, `stock_prices` | securities and their price history |
| `portfolio_items`, `portfolio_transactions` | positions and transactions of securities accounts |
| `exchange_rates`, `tax_exemptions`, `assets` | the remaining lists |
| `meta`, `warnings` | where the database came from and what the reader noticed |

| View | What it is for |
|---|---|
| `bookings_view` | bookings with the name, IBAN and kind of their account and the levels of their category |
| `account_balances` | the stored balance of each account next to the sum of its bookings |
| `monthly_category_sums` | counted, booked bookings summed per month, category and currency |

The complete reference, every column with type and description, is in
[docs/schema.md](docs/schema.md). It is generated from the same definition as
the database, so it cannot drift.

You can open the file with any DuckDB client as well:

```console
$ duckdb -readonly b4.duckdb "select * from bookings_view limit 5"
```

## For agents and scripts

- Give an agent the database, not the vault. It needs no password and cannot
  reach the bank access data, because that never leaves the vault.
- With an askpass program, an agent can run `b4 decrypt` itself when the
  database is out of date: the password is asked for in a dialog and goes to
  `b4` directly. This keeps the password out of the agent's transcript; it is
  no protection against an agent that is after it
  ([Asking in a dialog](#asking-in-a-dialog-when-there-is-no-terminal)).
- `b4 schema -o json` describes every table and column in one call.
- `b4 sql` opens the database read-only, with DuckDB's file system access
  switched off and its configuration locked: a query can read this database
  and nothing else. It cannot read or write other files, load extensions,
  reach the network or change the data. The test suite runs every escape we
  know of against it.
- All commands open the database read-only, so several can run at once.
- `b4 status --check` in a script: exit `0` means the database matches the
  vault as it is now.

## What b4 reads and what it never reads

A Banking4 vault holds two very different kinds of data: your financial
records, and what Banking4 needs to log in at your banks. `b4` is built around
keeping the second kind where it is.

**Never read.** These entries of a vault are skipped by name before a single
byte of their content is decrypted. The reader works from a list of entries it
knows how to decode and skips everything else. Before it decrypts anything, it
also reserves the storage of every entry it is not going to read, and refuses a
vault in which an entry it reads leads into that storage. That catches a
damaged directory; it is not a defence against someone who deliberately
rewrites a vault (see the last point of this section).

| Entry | What it holds |
|---|---|
| `Contacts`, `EbicsContacts`, `ContactsHTML`, `SpoolerContacts` | bank access: user ids, PINs, keys |
| `OnlineContactKeys` | key pairs and passwords for online access |
| `Outbox`, `OutboxData` | orders waiting to be sent, with their authorisation data |
| `ModelSettings` | application settings, among them a key and API tokens |
| `OnlineTraces` | protocol logs of bank communication |

**Dropped while decoding.** A few fields inside the account and payment
records belong to bank access as well (the online banking login id, a download
continuation token, references to the stored bank access, TANs). They are
skipped over while a record is decoded: they are never turned into values,
never stored and never written anywhere. The decrypted bytes of a list are
wiped from memory once it has been decoded.

**Read and exported.** Accounts, bookings, payments, standing orders, payees,
categories, mandates, exchange rates, securities and portfolio positions:
names, IBANs, amounts and purposes in full.

Keep in mind what follows from that:

- **The database is not encrypted.** It contains your bookings in plain form.
  Treat `b4.duckdb` like a bank statement: keep it out of repositories and
  shared folders, and delete it when you are done. (`*.duckdb` in your
  `.gitignore` is a good idea.)
- The vault's encryption itself is Banking4's. `b4` does not weaken it and
  cannot strengthen it: whoever has the vault file and the password can read
  it, with or without this tool.
- **Decrypt only vaults nobody else could have modified.** The format has no
  integrity protection, and `b4` recognises bank access data by the names
  Banking4 gives its entries. It refuses a vault in which a readable entry
  leads into the storage of an entry it does not read, but it cannot tell when
  an entry has been renamed. The guarantees above hold for vaults as Banking4
  wrote them ([SECURITY.md](SECURITY.md)).

## Compatibility and how it was verified

There is no public documentation of the vault format. It was worked out for
interoperability with a licensed copy of Banking4 and is written down in
[docs/format](docs/format/README.md), in our own words. `b4` is implemented
from that description.

How far the checks go:

- **One real vault, compared with data exported from Banking4.** About 33,000
  records with 1.4 million values in eleven tables: accounts, bookings,
  payments, standing orders, categories, payees, exchange rates, stock
  instruments and prices, portfolio items, tax exemptions. No differences.
- **Test files.** Nine containers that cover the encryption variants and
  several hundred synthetic records of the record types `b4` decodes, produced
  with Banking4, plus damaged and unusual cases with their expected results.
  Mandates, account groups, payers, assets and portfolio transactions are
  verified on these only: the real vault had none of them, except mandates,
  which the exported data did not cover.
- **A complete demo vault** of an invented household
  ([tests/fixtures/vault](tests/fixtures/vault/README.md)), produced with
  Banking4, is decrypted and queried by the test suite on every run.

The records in a vault carry no format version a reader could go by (the
container has a single version byte, and `b4` refuses any value but the known
one). Banking4 extends the format by appending fields to records, and `b4` is
built for that: a record that is longer than expected is decoded as far as it
is known and the extra bytes are counted in a warning. A new Banking4 version
that only adds fields therefore does not break the reader, and you can see
when something new appeared; a version that changes the format in another way
would need a new `b4`. The real vault used for testing was written by a newer
Banking4 version than the one the format description is based on and already
shows this (a few
unknown bytes per account, payee and portfolio record).

What that leaves open is listed, most important first, in
[docs/format/README.md](docs/format/README.md#open-questions-and-unverified-claims).

## Limitations

- **Read-only, by design.** `b4` cannot change a vault, create bookings or talk
  to a bank.
- **One real vault.** Everything above was confirmed against a single real
  vault (German banks, EUR accounts). Vaults from other
  editions, platforms or much older versions may contain things `b4` has not
  seen. It will warn rather than guess, and a report with the output of
  `b4 status` is very welcome.
- **Device-bound vaults** (protected with a device key in addition to the
  password) cannot be opened.
- **Not exported yet:** account documents and their attachments, invoices,
  the signature folder, saved filters, payee pictures.
- **Not reproduced:** values Banking4 computes on the fly, such as the running
  balance column, the next execution date of a standing order, and the current
  value of a portfolio position.
- **Times have no zone.** Banking4 stores most timestamps without one; `b4`
  passes them on as they are.
- Decrypting while Banking4 has the vault open gives you what is on disk at
  that moment. If Banking4 is in the middle of writing, `b4` may report a
  damaged vault: try again after Banking4 has finished.
- `b4 sql` has no time limit: a query that never ends has to be stopped by
  you (Ctrl-C, or `timeout 60 b4 sql …` in a script).

## Development

```console
$ nix develop          # or: any Rust toolchain plus go-task
$ task test            # unit and integration tests
$ task lint            # clippy, warnings are errors
$ task fmt
$ task docs            # regenerate man/ from the clap definition
```

Layout:

| Path | What |
|---|---|
| `src/vault/` | the encrypted container: header, key derivation, allocation table, directories |
| `src/wire/` | record framing and primitive values |
| `src/model/` | the record types, decoded into plain structs |
| `src/load.rs` | reads a vault into the model; the list of entries b4 reads lives here |
| `src/db/` | the DuckDB schema, writer and read-only access |
| `src/commands/`, `src/cli.rs`, `src/output.rs` | the command line |
| `docs/format/` | the format description |
| `tests/fixtures/` | synthetic test data: containers, records and a complete demo vault |

The tests need no Banking4 installation and no real data.

### Releasing

1. Bump `version` in `Cargo.toml` (authoritative) and `package.nix`, run
   `cargo build` so that `Cargo.lock` follows, and `nix build .#b4`: if it
   reports a different `cargoHash`, copy that into `package.nix`.
2. `task docs` and commit the regenerated man pages (they carry the version).
3. In [CHANGELOG.md](CHANGELOG.md), turn `[Unreleased]` into
   `[<version>] - <date>`. Run `task check`.
4. Commit as `release: <version>`, tag `v<version>` and push both.
5. Publish a GitHub Release for the tag, with the changelog section as its
   text (`gh release create v<version> --notes-file <section>`). The
   [release workflow](.github/workflows/release.yml) then builds the binaries
   for Linux and macOS (x86_64 and aarch64), tests each on the demo vault and
   attaches the archives with a `SHA256SUMS` file. It refuses a tag that does
   not match the version in `Cargo.toml` or has no changelog section.

Running the workflow by hand (`gh workflow run release.yml`) is a dry run: it
builds and tests the same archives and keeps them as workflow artifacts. Add
`-f only=linux` or `-f only=macos` to build a part of them.

## License

MIT, see [LICENSE](LICENSE).

Banking4 is a product of Subsembly GmbH. All product names are the property of
their owners and are used here only to say what this tool reads.
