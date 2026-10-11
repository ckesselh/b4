# The Banking4 vault format

This directory describes the file format of Banking4 data vaults (`.sub`) as
far as a read-only tool needs it. It is the specification `b4` is implemented
from.

| Document | Subject |
|---|---|
| [01-container.md](01-container.md) | The container: header, key derivation, allocation table, directories, encrypted file contents |
| [02-records-wire-format.md](02-records-wire-format.md) | How a file is cut into records and chunks, and every primitive value |
| [03-model-and-accounts.md](03-model-and-accounts.md) | Which entries a vault holds (the file map), the record base, accounts, groups, saved filters, settings |
| [04-payments-and-bookings.md](04-payments-and-bookings.md) | The payment base, bookings, payments, standing orders, the outbox |
| [05-master-data-and-portfolio.md](05-master-data-and-portfolio.md) | Payees, categories, mandates, exchange rates, securities, documents and the remaining lists |
| [database-mapping-notes.md](database-mapping-notes.md) | The decisions and value conventions behind the database: signed amounts, what counts, keys, time zones, rounding, what is left out. Not part of the format. The tables and columns are in [../schema.md](../schema.md). |

Each of the five format documents ends with a verification section and its
own open questions. This page gives the overview, the rules for coping with
vaults written by other program versions, and one list of everything that is
still open, ordered by how much it can hurt.

## The format in one page

**Container (01).** A vault is one file holding a small file system. It
starts with a 512-byte header (magic `53 55 42 01`) in a slot of one cluster;
cluster `n` is at offset `(n + 1) * cluster_size`. An allocation table with
one 4-byte entry per cluster chains clusters into files and directories. A
directory is a chain of pages of 64-byte entries (first cluster, size, flags,
name of up to 54 UTF-8 bytes). The header, the table and the directories are
not encrypted: names and sizes are readable without the password. File
contents are encrypted per file, when the entry's flag says so, with AES in
ECB mode under one content key for the whole container. That key is stored
in the header, wrapped with a key derived from the password: RIPEMD-160 over
a constant and the UTF-16LE password, expanded to 32 bytes, then stretched
with `kdf_rounds` rounds of AES-256 over a salt. Nothing in the container is
integrity protected; only the password check detects a wrong key. All
integers are little-endian.

**File map (03, Part 1).** Banking4 keeps its data in the root directory and
exactly one level below it. Global lists are root files (`AcctsNew`,
`AcctGroups`, `Payees`, `Categories`, `Mandates`, `ExchangeRatesBIN`,
`StockInstruments`, ...). Everything that belongs to one account is in a
directory `Acct<oid>` (`Stmt`, `Paymts`, `Stos`, `Portf`, `PortfTxns`,
`AcctTaxExemptions`, ...), and the price history of a security is in
`Stock<oid>/StockPrices`. A file that was never needed does not exist, and a
missing file is an empty list. The type of a file follows from its name and
place alone: no file carries a magic number or a version. Nine entries hold
bank access data or secrets and are never read by `b4`: `Contacts`,
`EbicsContacts`, `ContactsHTML`, `SpoolerContacts`, `OnlineContactKeys`,
`Outbox`, `OutboxData`, `ModelSettings` (secrets among harmless preferences)
and `OnlineTraces` (see "Never" below).

**Record framing (02).** A collection file is nothing but records back to
back: `[i32 record_size][body]`, where the size does not count itself. A
body is one or more chunks: `[u32 header][payload]`, where the low 30 bits of
the header are the chunk size including the header and bit 31 says that
another chunk follows. Fields are read in a fixed order with no tags. A read
that does not fit into the rest of its chunk returns the type's default and
does not move, and bytes left over at the end of a chunk are skipped. That
pair of rules is the whole versioning mechanism: newer versions append
fields, older records are simply shorter. The primitives are `u8`, `bool`,
`u16`, `i32`, `decimal` (the 16-byte .NET layout), `string` (length-prefixed
UTF-8, empty equals null), `code(n)` (fixed width), `blob`, `guid`, three
readers for a 4-byte calendar date (`date_raw`, `date_clamped`,
`date_strict`), `ticks` (100 ns units since year 1, no zone) and `time_hms`.
The format has two more that no record uses: `tristate` and a plain `i64`.

**Record types.** Every record starts with the record base: an object id and
an update stamp, both `i32` seconds since 2000-01-01 UTC. Five record types
have two chunks; all others have one.

| Record | File | Layout | Chunks |
|---|---|---|---|
| account | `AcctsNew` (legacy `Accts`) | 03 | 2 |
| account group | `AcctGroups` | 03 | 1 |
| booking filter | `AcctStmtFilters` | 03 | 1 |
| setting | `ModelSettings` | 03 | 1 |
| booking | `Acct<oid>/Stmt` (legacy also `StmtMasterEntries`) | 04 §5, §6 | 2 |
| payment | `Acct<oid>/Paymts` | 04 §5, §8 | 2 |
| standing order | `Acct<oid>/Stos` | 04 §5, §9 | 2 |
| outbox order | `Outbox` | 04 §10 | 1 |
| payee, payer | `Payees`, `Payers` | 05 §3.1, §3.2 | 1 |
| category | `Categories` | 05 §3.3, §4 | 1 |
| mandate | `Mandates` | 05 §3.4 | 1 |
| exchange rate | `ExchangeRatesBIN` | 05 §3.5 | 1 |
| stock instrument, stock price | `StockInstruments`, `Stock<oid>/StockPrices` | 05 §3.6, §3.7, §5 | 1 |
| portfolio item, portfolio transaction | `Acct<oid>/Portf`, `PortfTxns` | 05 §3.8, §3.9, §5 | 1 |
| tax exemption | `Acct<oid>/AcctTaxExemptions` | 05 §3.10 | 1 |
| asset | `Acct<oid>/Assets` | 05 §3.11 | 1 |
| account document | `Acct<oid>/AcctFiles` | 05 §3.12 | 1 |
| signature folder item, split | `Acct<oid>/AcctPayChecks`, `AcctPayCheckSplits` | 05 §3.13, §3.14 | 2, 1 |
| invoice | `Invoices` | 05 §3.15 | 1 |
| online trace, import/export log entry | `OnlineTraces`, `ImportExportPaymtsLog` | 05 §3.16, §3.17 | 1 |

Bookings, payments, standing orders and signature folder items share the
payment base (04 §5): amount without sign, a direction, counterparty,
purpose text, category and the SEPA fields. Records refer to each other by
object id (account to group, booking to its split or batch master, payment
to booking) and to categories by full name as text; there are no category
ids.

Decoding the tables is not the whole job. For several record types
Banking4 changes values after reading them (the direction and scheme of a
payment, the currency and status of a booking, the regrouping of splits and
batches, the normalisation of category names). Each document marks those
rules and says which ones a reader has to replicate; 04 §12 has a checklist.

### Names that differ between the documents

| Thing | 02 | 03 | 04 | 05 |
|---|---|---|---|---|
| The two leading fields of every record | "object id" and "update stamp" | record base | object header | object header |
| Records of `AcctPayChecks` | signature folder item | pay check; the table of entries gives both names | signature folder item | signature folder item |
| Records of `OnlineTraces` | | online trace, also "trace entry" | | online trace |
| The booking's kind of transaction | | transaction kind | `transaction_code` | |
| The colour mark of a booking | | booking colour flag | `flag`, `color_flag` | |
| A missing text | null | null | "no value" | "no value" |
| The owner of the format | Banking4; "the reader", "the writer" | Banking4, the application; "the reader", "the writer", "the loader" | Banking4, the application; "the reader", "the writer" | Banking4, the application; "the reader", "the writer", "the loader" |

Where two documents state the same thing differently, 02 is the reference
for the wire format and 01 for the container.

## What these documents describe and what is confirmed

The format was learned for interoperability, from one licensed Banking4
installation. The documents describe layouts, algorithms and meanings in our
own words and with our own field names. They contain no code from Banking4.
No part of Banking4 is in this repository and `b4` uses none.

The documents describe one Banking4 9.x build for Windows (program files
dated 2026-05-18). It writes account records with record version 26. The
documents and this page call it "the Banking4 version described here".

**Test files.** The test files are in `tests/fixtures/`. The containers,
the wire samples, the well-formed records of the record files and the demo
vault were produced with Banking4 from synthetic values and hold the bytes
and the expected values. The reader cases of 02 and the remaining records
of the record files of 04 and 05 (04 §13.1, 05 §8.1) are cases with their
expected results. They cover forms Banking4 does not write: records that
end early, malformed records and collection files, and values outside the
defined ones. What Banking4 does with a damaged container is listed in
01 §13.2. The tests of `b4` add damaged containers and damaged records of
their own.

| Document | Test files in the repository | Confirmed on them | Not confirmed |
|---|---|---|---|
| 01 | 9 containers, 464 entries | key derivation, tables, directories, file contents | write order inside one step of a write, file sharing and locking, limits |
| 02 | 40 samples; 579 reader cases with their expected results | framing, and every primitive read and written, including reads past the end of a chunk | what Banking4 does around the reading of a file's records when it loads the file (opening it, handling errors), the choice between the binary and the XML form |
| 03 | 63 records | the five record layouts of Part 2 and their loading rules | all of Part 1: file map, `Accts` migration, load order, settings keys |
| 04 | 255 records | payment base, booking, payment, standing order, their adjustments and collection rules | split editing row and outbox (no test files in the repository); cross entries, displayed transaction code, schedule |
| 05 | 209 records | twelve record types (3.1 to 3.12), identifiers, splits, category rules | all loading rules; the five record types of 3.13 to 3.17 (no test files in the repository) |
| 01 to 05 together | the demo vault `tests/fixtures/vault/demo.sub` with `MANIFEST.json`: 45 entries, 968 records of 14 record types in 21 collection files, and 14 entries a reader must leave unread (decoys under the never-read names, two unknown names, the `Accts` placeholder) | a whole synthetic vault, and the lists after loading, with Banking4's drop, change and sort rules | well-formed content only; a synthetic vault, not one from normal use; what Banking4 does with a vault when it starts |

Banking4's default product variant is not the full edition: Banking4 loads
portfolio transactions, payers, batches and invoices only when the manifest
parameter for the product variant selects the professional or the business
edition (03, "What is loaded when"). The record layouts do not depend on
the variant. The expected results in the record test files of 03 to 05 are
those of the default variant; those of the demo vault are those of the
professional edition.

For the Banking4 version described here the tables of 03 to 05 are complete:
each lists every field of its record type, with its primitive. Three more
statements hold for every record type documented there: only five have more
than one chunk, only two fields treat a `u16` as signed, and none uses
`tristate` or plain `i64`.

**Real vaults.** One real vault, written by a newer program version than the
Banking4 version described here (512-byte clusters, security flags value 2),
was read with the container reader of `b4`. This is confirmed against data
exported from Banking4: all 92 files that were compared came out byte for
byte the same.

The record decoders of `b4` were compared, value by value, with data
exported from Banking4 for the same vault:

| Record | Records | Document |
|---|---|---|
| account | 21 | 03 |
| booking | 21,931 | 04 §13.6 |
| standing order | 25 | 04 §13.6 |
| payment | 3 | 04 §13.6 |
| category | 51 | 05 §8.6 |
| payee | 455 | 05 §8.6 |
| exchange rate | 51 | 05 §8.6 |
| stock instrument | 43 | 05 §8.6 |
| stock price | 10,419 | 05 §8.6 |
| portfolio item | 5 | 05 §8.6 |
| tax exemption | 12 | 05 §8.6 |

That is about 1.4 million compared values without a difference. The
database `b4` writes from the vault was compared with the exported data as
well and equals it in all 1.34 million compared values of accounts and
bookings. No value of the vault was copied into these documents; they record
counts and field names only.

What that vault showed:

- 837 bookings, the whole statement file of one account, and all 3 payments
  are in the legacy single-chunk layout (04 §5.2), none of them with a
  tagged purpose line. Such records exist and must be decoded.
- Account records carry record version 27 with 10 unread zero bytes; payee
  and portfolio item records each carry 2 unread zero bytes after the last
  known field; category records store layout version 8 and end one byte
  before the table does (03; 05 §8.6).
- It has a `Mandates` file with 53 records, which decode without an error
  but have no cross-check, because the Banking4 export used for comparison
  had no mandates.
- It has no `Payers`, `PortfTxns`, `Assets` and `AcctGroups` files, and no
  splits, batches or hidden payments. Those record types and rules are
  confirmed on the test files of the repository only.

The comparison shows that stored values decode correctly in one vault of
one program version. It reaches no further than the exported data does: the
export holds nothing for the unread bytes, so it says nothing about what
they hold, and what the export did not contain was not compared.

**The demo vault.** `tests/fixtures/vault/demo.sub` is a complete synthetic
vault with invented content, produced with Banking4, with the header values
of the real vault. Its manifest records the entries of the vault and the
expected result of loading each file. It confirms, inside a complete
container, several points that the other test files do not: the kind-0
placeholder in `Accts` is ignored when `AcctsNew` exists (03); a correctly
written split and a correctly written batch survive the sort and regrouping
of the bookings unchanged (04 §6.4); the special categories are not
duplicated when all three exist (05 §3.3); the lists are kept in the sort
orders given in 05 (payees, exchange rates, portfolio items); and Banking4
reads the settings file as a side effect of loading the exchange rates
(03). A value Banking4 replaces by zero while loading is a plain `0` with
scale 0, not `0.00` (03, 05). The vault holds only well-formed records, so
it says nothing about the loading rules for damaged or unusual content.

## Coping with other versions

The format has no version number, so a reader cannot ask what it is looking
at. It has to accept what Banking4 accepts, and for a read-only tool it
should accept more. The rules, with the place where each is
specified:

| Situation | What the reader does | Warn? |
|---|---|---|
| A chunk ends before the last field of the table (older writer) | Missing fields take their defaults, exactly by the rules of 02 §3, including the special cases for strings and `date_strict` | no |
| Bytes are left at the end of a chunk (newer writer) | Skip them | count per record type, report once |
| A record has more chunks than the reader knows (newer writer) | Decode the known chunks, skip the rest. Banking4 rejects the whole file here; `b4` deliberately does not (02 §8 rule 6). This holds for structurally valid chunks only: every announced chunk is checked, and a malformed one is the out-of-bounds case of the last row, even if the decoder never visits it or the record is undecodable for another reason | yes, per file |
| Unused bytes between the last chunk and the end of the record | Skip them | no |
| 1 to 3 stray bytes at the end of a collection file | Ignore them | no |
| A missing collection file | Empty list | no |
| A payment-family record with a single chunk (legacy layout, 04 §5.2) | Decode it by 04 §5.8: real vaults contain such records. Never read the extension fields from it | count per file, report |
| An unknown entry in the root directory or in an `Acct<oid>` or `Stock<oid>` directory | Skip it without reading it | yes, by name |
| One of the nine entries that are never read (see "Never") | Skip it without reading it | no; at most a count |
| An entry that is not read (never read, not exported or unknown) whose own chain is damaged | Reserve as much of its storage as its chain yields and carry on (01 §6.3) | no |
| An entry the reader decodes whose chain leads into the storage of an entry it does not read, or an `Acct<oid>` or `Stock<oid>` directory on the pages of such a directory | Refuse the vault: read nothing of that entry, write no database (01 §6.3) | error: the vault is damaged or was manipulated |
| Several stock instruments with the same canonical ISIN | Keep them all (05 §3.6) | yes, a count |
| A split line of a stock instrument that does not parse | Keep the record, its other splits and its raw text (05 §3.6.1) | yes, a count per file |
| A directory `Acct<oid>` or `Stock<oid>` whose id matches no account or instrument (an orphan) | Decode it by its file names all the same. Booking adjustments that need the account are applied as for an account without a currency (03, "Entries") | yes |
| An unknown value of an enumeration, or undefined bits in a flag set | Keep the number | no |
| A value Banking4 would fail on (invalid decimal, impossible strict date, bad time, out-of-range ticks, over-long blob) | Report the record as undecodable with file, record index and field; carry on with the next record | yes |
| A repeated object id within one file | Keep both records, each with the id it has | yes |
| A `date_raw` that is not a real calendar date (partial, impossible, negative year) | Keep the record and the parts as read; the database stores NULL (02 §4.12) | yes, count per table and column |
| A list count that the rest of its chunk cannot hold | Never allocate by it; the record is undecodable (02 §8 rule 11) | yes |
| A string longer than the rest of its chunk | No value; the rest of that chunk reads as defaults, the next chunk normally (02 §3) | count per record type |
| A pending booking dated 9999-12-31, on which Banking4's sort fails | Sort keys saturate; nothing fails (04 §6.4) | no |
| A record size or chunk size that is out of bounds, a truncated record, in a chunk the decoder reads or in one it never visits | Stop reading that file; keep the records before it. Do not resynchronise | yes |

**Old and new file pairs.** These are the places where a name changed:

| Old | New | Rule |
|---|---|---|
| `Accts` | `AcctsNew` | `AcctsNew` wins whenever it exists. Then `Accts` holds a single placeholder record and is ignored. Without `AcctsNew`, `Accts` holds the accounts. Records of account kind 0 are skipped in either file (03) |
| `Acct<oid>/StmtMasterEntries` | `Acct<oid>/Stmt` | Both are booking files with the same layout. Read both; the legacy file holds further bookings of the account (03, 04 §1) |
| `ExchangeRates` | `ExchangeRatesBIN` | Banking4 never reads the old file, it only deletes it. Its layout is unknown: skip it unread. The name is known, so it is not reported as an unknown entry |
| `Traces` | `OnlineTraces` | `Traces` is obsolete like `ExchangeRates`: skip it unread. `OnlineTraces` is not a fallback for it: it is one of the nine entries `b4` never reads |
| `Layout`, `TanLists` | (none) | Obsolete, layout unknown: skip them unread |

**Warnings** should name the file (and for a record its index and object
id), never a value from the record. One warning per file and kind is
enough; a count says more than a thousand lines. Because the Banking4 version
described here is older than the program that wrote at least one real vault, it
is worth recording per record type how many records had bytes left over and how
many, so that a format change shows up as a number instead of going unnoticed.
A strict mode that turns the tolerated cases into errors is useful for testing.

**Never.** These rules have no exceptions and no option that turns them off.

- **Never write.** A reader opens the vault for reading only and writes
  nothing (01 §11).
- **Never read the bank access entries.** Nine entries of the root directory
  are skipped by name before a byte of their content is read, whether they
  are files or directories: `Contacts`, `EbicsContacts`, `ContactsHTML`,
  `SpoolerContacts`, `OnlineContactKeys`, `Outbox`, `OutboxData`,
  `ModelSettings` and `OnlineTraces`. The first five are the stored bank
  accesses; the outbox holds user ids, TANs and bank contacts of orders;
  the settings hold a crypto key and API tokens among preferences; the
  traces are bank protocol logs. The safe implementation is an allow-list:
  only the content of entries the reader decodes is read at all (03,
  "Entries").
- **Never decrypt anything before the storage of what is not read is fenced
  off.** Skipping by name protects a name, not the bytes: a directory slot
  that keeps an allowed name and is given the first cluster of a
  bank access entry would have the reader decrypt that entry under the
  allowed name, because nothing in a container says whose a cluster is and
  all files share one key. So before the content of the first file is read,
  the reader reserves the clusters of every entry it does not read (the nine
  above, those it knows and does not export, and those it does not know; in
  the root and in the `Acct<oid>` and `Stock<oid>` directories), following
  only the allocation table and, for a skipped directory, its unencrypted
  pages. An entry it does read whose chain reaches a reserved cluster is
  refused with nothing of it read, and the vault with it (01 §6.3).
- **Never keep the access fields of decoded records.** A decoder reads them
  only to advance and drops them at decode time, so that they are never held
  in a memory structure, never stored and never exported. Of the account:
  `fints_customer_id`, `last_download_token`, `contact_guid`,
  `order_contact_guid`, `contact_name`, `order_contact_name` and
  `fints_account_sub_id` (03). Of the payment: `used_tan` and
  `order_reference_tan` (04 §8.1). Of the account document, from the day it
  is implemented: `acknowledge_token` (05 §3.12).
- **Never touch the host file system on behalf of a name stored in a
  vault.** Trace file names and document file names are text; a reader does
  not open, test or list them (05 §3.12.1, §3.16.1).
- **Never trust an unprotected number with memory or time.** Not the round
  count of the header (at most 10,000,000, 01 §5.2), not a directory entry's
  size (01 §8.1), not a list count (02 §8 rule 11); and never return the
  same clusters twice, within one chain or for two entries, never return an
  allocation table page as contents, never walk a damaged chain once per
  entry that leads into it, and never walk the storage of a skipped entry
  more than once (01 §6.3).

## Open questions and unverified claims

One list for all five documents, most dangerous first. "Risk" is the risk
that `b4` shows wrong or incomplete data without noticing. The reference in
brackets is the document and its open question or section.

### A. Can make decoded data wrong or incomplete

1. **Records written by a newer program version.** The account records of
   the real vault carry record version 27 and ten unexplained bytes per
   record after the last known field, and its payee and portfolio item
   records two; all were zero. The Banking4 version described here ignores them
   and the data exported from Banking4 holds nothing for them, so what they are
   is unknown. A reader skips such bytes safely, and every known field of these
   records decodes to the value in the Banking4 export, but what the bytes hold
   is missing from its output. [03 q1, 05 q12, 04 q12]
2. **One real vault only.** The container test files and the demo vault
   are synthetic: produced with Banking4 from invented content, not vaults
   from normal use. The one real vault that is known has 512-byte clusters
   and security flags value 2, which the Banking4 version described here, a
   Windows build, never produces, so it was written by another build or
   platform. Its container reads correctly and eleven of its record types
   decode to the values in the Banking4 export (see above). Vaults of other
   builds, platforms and program versions are not confirmed, and neither,
   on real data, is anything that vault does not contain. The demo vault
   has the same header values as the real vault. [01 q2, q13, 13.4]
3. **Legacy single-chunk records.** Bookings, payments and standing orders
   exist in an older layout in which most fields are packed into tagged
   purpose lines. That real vaults contain such records is answered: the
   real vault holds 837 bookings and 3 payments in that layout, so a reader
   must decode them (04 §5.8), and `b4` does, to the values in the Banking4
   export. What is still open is narrower: none of those records has a
   tagged purpose line, so the tags are confirmed on the test files of the
   repository only, and which program versions wrote the layout is
   unknown. [04 q1, 02 q3]
4. **Time zone of `ticks` values.** The encoding carries no zone and it is
   not known, per field, whether Banking4 stores UTC or local wall-clock
   time. Affects the account's balance and sync times, exchange rate quote
   times, portfolio transaction dates, tax exemption validity, document
   dates and others. Several are probably plain dates at midnight. Stamps
   (`oid`, `update_stamp`) are not affected: they are UTC. [02 q1, 03 q3,
   05 q1]
5. **Rules that depend on the installation that wrote the vault.** Three
   reader rules use values that are not in the vault: the language of the
   pseudo category names in category normalisation, the application's
   default currency for bookings of an account without a currency, and the
   table of crypto currency codes that turns a cash account into a crypto
   account. All three only matter for unusual stored values. [04 q2, q3,
   q14, 03 q6]
6. **Loading rules that are not confirmed.** Which records Banking4 drops
   or changes while loading a file is confirmed on the test files of the
   repository for accounts, groups, filters, settings and the payment
   family, but not for the lists of 05: dropped categories and the
   de-duplication of special categories, the inversion and filtering of
   exchange rates, the mandate filter, the failure on a duplicate ISIN.
   The whole of 03 Part 1 (file map, the `Accts` migration, what is loaded
   when) is not confirmed either. The demo vault confirms a few of these
   on well-formed content (the ignored placeholder in `Accts`, no
   duplicated special categories, the sort orders); every rule about
   dropped, changed or damaged records is still not confirmed.
   [05 q10, 8.4; 03 "Not confirmed"]
7. **Record types without test files.** The split editing row, the
   outbox order, the signature folder item and split, the invoice, the
   online trace and the import/export log entry have no test files in the
   repository, so less is confirmed for them than for the other record
   types (05 §8.5 says what is confirmed for the last five). How Banking4
   reads such a record when it ends early is not confirmed for any of
   them. None of them is planned for export. [04 q16, 05 8.5]
8. **Real data for record types without a cross-check.** Payers, assets,
   portfolio transactions and account groups were never compared with a
   file a real installation wrote: the real vault has none, and they are
   confirmed on test files produced with Banking4 only. Mandates were read
   from the real vault (53 records) but the Banking4 export used for
   comparison had no mandates, so their values were not compared. Account
   documents are neither in that export nor read by `b4`. Splits, batches
   and hidden payments do not occur in the real vault either.
   [05 8.4, 8.6; 04 13.6]
9. **Reading while Banking4 has the vault open.** Between the steps of a
   write the container is consistent (01 §7.5, §11). Whether it is
   consistent inside one step is not confirmed, and neither is what a
   reader sees of a vault that Banking4 has open in normal use, which may
   also differ between the platforms Banking4 runs on. A reader should
   treat chain and table errors on a vault in use as "try again", and `b4`
   should read a copy or retry. [01 q10]
10. **Platform differences.** Some details are decided by the runtime
    Banking4 runs on rather than by Banking4 itself. They may differ
    between the platforms Banking4 runs on and are not confirmed for all of
    them: the count of replacement characters for malformed UTF-8, white
    space trimming beyond the first 256 code points, the integer parser of
    the retention period text, the sort order of category rule keywords.
    None affects data Banking4 writes. [02 q8, 03 q13, 04 q12]
11. **Older program versions.** These documents describe one version of
    Banking4. The defaults of 02 §3 say what that version makes of a shorter
    record, not which fields older versions really wrote or what they stored
    in fields that are now unused. [02 q3, 03 "Not confirmed", 05 q12]
12. **Editions.** Branded editions switch off whole lists and some fields,
    and so does the product variant of the ordinary product: its default
    variant, whose results the record test files hold, does not load
    portfolio transactions, payers, batches and invoices; the professional
    and the business edition do. The demo vault holds the results of the
    professional edition. What else depends on the variant, and what a
    vault of each variant looks like, is not known. [03 q15]
13. **Unparseable stored texts.** Banking4 fails on a retention period
    text it cannot parse, on a malformed split line of a stock instrument
    and on a payee verification text that is not the expected JSON, and
    then stops loading that file. Whether any program version
    wrote such texts is unknown. `b4` keeps the raw text and carries on.
    [03 q12, 05 3.6.1, 04 8.5]

### B. Stored values decode correctly, derived values cannot be reproduced

14. **Standing order schedule.** The rule by which Banking4 computes the
    next execution date is not specified in these documents, so the next
    date and the list order Banking4 shows for standing orders held at the
    bank cannot be reproduced. [04 q15]
15. **Displayed transaction code.** The lookup tables from purpose code,
    business transaction code and PayPal text are not specified in these
    documents; a reader stores the raw code. [04 q4]
16. **Cross entries.** The link between the two sides of an internal
    transfer is not stored and is found again by a heuristic that is only
    partly specified. [04 q5]
17. **Category rule matching** and the meaning of the markers `#`, `?` and
    `*`; the order of rules and keywords under other language settings.
    [05 q7]
18. **Portfolio values.** The value formula, the split adjustment of the
    purchase price and the maintenance of the instrument list are not
    confirmed. `b4` computes none of them and applies no split
    adjustment or rate inversion to a stored value. [05 8.4]
19. **Sign conventions.** Whether an expense budget is stored positive or
    negative; whether `instructed_amount` and `charges_amount` of a booking
    can be negative; whether `amount` was ever written negative.
    [05 q6, 04 q7, q8]
20. **JSON texts.** The structure of the outbox order's payee verification
    text and the exact conversions of Banking4's JSON parser. [04 q6]
21. **How common the odd cases are:** pending bookings without a date,
    payments without a currency, account kinds with more than one bit.
    [04 q11, q17]
22. **Running balance.** The balance after each booking and at the end of
    each day is not stored, and the rule by which Banking4 recomputes them
    is known only as one sentence that is not confirmed and leaves the
    essential choices open. `b4` does not reproduce it. [04 6.7]

### C. Meaning unknown, decoding unaffected

23. Fields named by interpretation or not understood: `portfolio_key`,
    `interest_method` (its code values), `mandate_date`, bit 0 of the
    signature folder item's flags and its order data, `data_id` of the log
    entry, the `debit_return_*` fields, `reserved_text` and the other
    unused fields, the values of `intl_remittance_method`,
    `regulatory_reporting_code` and `gvc_extension`. [05 q2 to q5, q8;
    04 q9, q10; 03 q4]
24. `security_flags` bit 1 (value 2): set in a real vault; nothing in the
    Banking4 version described here is known to depend on it. [01 q1]
25. The 20-byte `legacy_area` of the header, the header digest that
    nothing uses, the fourth magic byte as a possible version, bit 30 of
    the chunk header. [01 q3, q4; 02 q2]
26. Whether `b4` should check the `type_guid`. The one real vault that is
    known is reported to carry the GUID the application's configuration
    names; no other vault was looked at. [01 q6]
27. Why the account file was renamed to `AcctsNew`, and what the constant
    second chunk of the account record is for. [03 q2]
28. A dormant XML form of the categories file in Banking4; whether old
    vaults can hold an XML categories file. [02 q4]
29. Formats of the obsolete entries `Layout`, `TanLists`, `ExchangeRates`
    and `Traces`. [03 q10]
30. Entries or settings keys created by parts of Banking4 that these
    documents do not cover. [03 q8, q11]

### D. Outside what `b4` needs

31. Device-bound containers: where Banking4 keeps the device GUID. Access
    tokens: where they are kept. `failed_attempts`: who maintains it.
    [01 q7, q8, q9]
32. `cipher_id` 0, containers of 2 GiB or more or with more than 2^24
    clusters, broken names across platforms. [01 q5, q11, q12]
33. The amount parser behind a saved filter's search amount. [03 q14]
34. Document contents: that the content file holds the unmodified document
    bytes, and its extension table, are not confirmed. [05 q11]

### Settled in the review

For the record, questions the single documents raised and another document
or a further look answered: entry names are compared exactly and
case-sensitively (03 q9, by 01 §7.3); `StmtMasterEntries` is read as a
booking file (03 q7); the only signed `u16` fields are the booking's `gvc`
and `gvc_extension` (02 q6); signature folder items are specified in 05
(04 q13); the localised names of the special categories are listed in 05
§4.3 (05 q9); the path separator is a matter of the application and is not
stored in the container.

Settled by decision after two external reviews of these documents:

- The bank access fields of the account record are not exported: all seven
  are dropped at decode time, like the two TAN fields of the payment and
  the acknowledge token of the account document (03 q5; "Never" above).
- `Outbox`, `OutboxData`, `ModelSettings` and `OnlineTraces` are never read,
  like the five bank access entries (03, "Entries").
- A `date_raw` with a negative year is not "corrupt": the wire layer
  returns what Banking4 reads, the database stores NULL with a warning, the
  record stays (02 §4.12).
- An orphan `Acct<oid>` or `Stock<oid>` directory is decoded with a warning
  (03, "Entries").
- Sums count batch and split members and ordinary bookings and leave out
  masters that have members; counting masters instead is not equivalent
  (04 §6.4).
- An update stamp of 0 is "unknown", like -1 (02 §5.1).
- The reader is stricter than Banking4 in six places of the container: the
  round limit, no allocation table page in a chain, no cluster twice in a
  chain, no cluster shared between entries, failed reads remembered so that
  a damaged chain is walked once, no allocation by a stored size (01 §5.2,
  §6.3, §8.1).
- The database schema: no uniqueness constraints and no synthetic ids, free
  texts verbatim, the category key and its upper-casing rule, no table of
  odd values ([database-mapping-notes.md](database-mapping-notes.md)).
