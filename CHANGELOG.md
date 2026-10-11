# Changelog

All notable changes to b4 are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

First version. Nothing has been released or tagged yet.

### Added

- `b4 decrypt <VAULT> [OUT]`: reads a Banking4 vault natively and writes a
  DuckDB database (default `./b4.duckdb`). The password comes from
  `--password-stdin`, `B4_PASSWORD` or a prompt without echo; it is never a
  command-line argument and never stored. An existing database is replaced only
  after the new one is complete.
- `b4 info <VAULT>`: a vault's header (cipher, key-stretch rounds, id, times)
  without the password.
- `b4 status [--check]`: what the database contains, the reader's warnings,
  and whether the vault has changed since it was decrypted.
- Query commands: `accounts list|show`, `bookings list|show|summary`,
  `payees list`, `categories list`, `standing-orders list`, `portfolio list`.
- `b4 sql <QUERY>`: read-only SQL against the database, sandboxed so that a
  query can read this database and nothing else.
- `b4 schema [TABLE]`: tables and columns with their descriptions.
- `b4 export <DIR>`: one CSV or JSON-lines file per table.
- `b4 completions <bash|zsh|fish>`, man pages for every command.
- Output as `table`, `json`, `jsonl` or `csv` for every query command, with
  exact decimals in all of them.
- Tables: accounts, account groups, bookings, payments, standing orders,
  payees, payers, categories, mandates, exchange rates, stock instruments and
  prices, portfolio items and transactions, tax exemptions, assets.
- A description of the vault format under `docs/format/`, a synthetic demo
  vault and test fixtures.
- Nix flake with a package and an overlay.
- Release archives for Linux and macOS (x86_64 and aarch64) with the binary,
  the man pages and the shell completions.

### Security

- Entries of a vault that hold bank access data (`Contacts`, `EbicsContacts`,
  `ContactsHTML`, `SpoolerContacts`, `OnlineContactKeys`, `Outbox`,
  `OutboxData`, `ModelSettings`, `OnlineTraces`) are never read. The reader
  works from a list of entries it decodes and skips everything else by name.
  Before decrypting anything it reserves the storage of every entry it does not
  read, and refuses a vault in which a readable entry leads into that storage.
  This holds for vaults as Banking4 wrote them: the format has no integrity
  protection, so a deliberately rewritten vault is outside what b4 can check.
- Bank access fields inside account and payment records (login id, download
  token, references to the stored bank access, TANs) are skipped while
  decoding: never turned into values, never stored, never written. Decrypted
  list contents are wiped from memory after decoding.
- The vault is opened read-only and never written, locked or modified.
- The container reader is stricter than Banking4's own: it caps the number of
  key-stretch rounds, and refuses a file when the part of its allocation chain
  that is read loops, shares clusters with another entry or runs through the
  allocation table.
- `b4 sql` runs with DuckDB's file system access disabled, its configuration
  locked and a memory limit, on a database opened read-only.
- Text from a vault is cleaned of control characters and escape sequences
  before it reaches a terminal.
- `unsafe` code is forbidden in the crate.

### Not yet

- Device-bound vaults cannot be opened.
- Account documents and attachments, invoices, the signature folder, saved
  filters and payee pictures are not exported.
- Values Banking4 computes on the fly (running balance, next execution date
  of a standing order, portfolio value) are not reproduced.
