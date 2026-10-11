# 03 - Vault file map, base records and account records

This document describes what is stored where inside a vault, and the layout of
the records that every other record builds on: the common record header, the
account, the account group, the saved booking filter and the settings entry.

It builds on [01-container.md](01-container.md) (clusters, directories,
encryption) and [02-records-wire-format.md](02-records-wire-format.md)
(framing, chunks, primitives). Only what is needed to read this document is
repeated here; where the two disagree, 02 is the reference for the wire
format. Primitive names are those of 02.

Status of the statements below:

- The statements describe the Banking4 build of one Windows installation
  (program files dated 2026-05-18; it writes account records of version 26).
  The product build number was not determined. The text below calls it "the
  Banking4 version described here".
- **Confirmed** marks a statement that holds for the Banking4 version
  described here; see [Verification](#verification). Every record layout of
  Part 2 is in this state, confirmed on test files produced with Banking4;
  the fixtures are in `tests/fixtures/records/`.
- A statement that is neither marked as confirmed where it stands nor
  covered by the list under
  [Confirmed](#confirmed) is not confirmed.
  The larger items of that kind are listed under
  [Not confirmed](#not-confirmed), what is not known under
  [Open questions](#open-questions).

"The reader", "the writer" and "the loader" stand for Banking4 when it reads
a record, writes a record and loads a collection file. "A reader" is any
program that reads a vault, `b4` included.

All names of fields, record types and enum members are our own. Names of
entries inside the vault (`AcctsNew`, `ModelSettings`, ...) and setting keys
are literal on-disk strings.

## Conventions

### Wire primitives used in the tables

| Name | Bytes | Value | Read past the end of the chunk |
|---|---|---|---|
| `u8` | 1 | unsigned | 0 |
| `bool` | 1 | non-zero is true | false |
| `u16` | 2 | little endian, **unsigned** (never sign-extended) | 0 |
| `i32` | 4 | little endian, signed | 0 |
| `decimal` | 16 | four little-endian 32-bit words: low, middle, high of a 96-bit integer, then a flags word (scale in bits 16..23, sign in bit 31) | 0 |
| `date_clamped` | 4 | `u16` year, `u8` month, `u8` day; any part zero means "no date", and so does 0001-01-01; the year is unsigned and capped at 9999 (`FF FF 01 01` is 9999-01-01), month at 12, day at the last day of that month (02 section 4.12) | no date |
| `date_strict` | 4 | same bytes, read as three separate values; any part zero means "no date"; no clamping | no date |
| `ticks` | 8 | signed 64-bit count of 100 ns units since 0001-01-01 00:00:00, no time zone; 0 means "no time" | no time |
| `string` | 2+n or 6+n | `u16` byte length, or `0xFFFF` followed by an `i32` byte length; then UTF-8 bytes. Length 0 means null: an empty string is written as length 0 and therefore comes back as null | null |
| `code(n)` | n | n single-byte characters, padded with zero bytes; trailing zero bytes and surrounding white space are removed; empty means null | null |
| `guid` | 16 | 16 bytes in the Microsoft mixed-endian order (first three groups little endian) | all zero |

A `string` whose length runs past the end of the chunk is not an error for
Banking4: it yields null, and every later read in the same chunk yields
the default. The read position then lies beyond the end of the chunk, so the
bytes left must be computed in signed arithmetic or clamped at zero (02
section 3).

**Counted lists.** Two records of this document store a count followed by
that many strings: the account (fields 42 and 43) and the booking filter
(fields 12 and 13). Banking4 trusts both counts. For `b4` the same
rule applies to every counted list of the format (02 section 8, rule 11):
never allocate by the count; refuse a count that is larger than the bytes
left in the chunk divided by the smallest size of one element (2 bytes for a
`string`); make that comparison as a division or in checked arithmetic,
never as a 32-bit product.

### Records, chunks and tolerance

A collection file is a sequence of `[i32 size][record]`. A record is one or
more chunks; each chunk starts with a 32-bit header whose low 30 bits are the
chunk size including the header and whose top bit says that another chunk
follows. Field reads never cross a chunk boundary: once a chunk is exhausted
every further read returns the default from the table above, and bytes left
unread at the end of a chunk are skipped when the reader moves on. This is the
format's forward and backward compatibility mechanism, and the record layouts
below rely on it instead of testing a version number.

Two consequences matter for a reader:

- A record may be **shorter** than the layout (written by an older version).
  The missing fields take their defaults.
- A record may have **more bytes at the end of a chunk** than the layout
  (written by a newer version). They are ignored.

Banking4 is *not* tolerant of **additional chunks**: if a record still has
an unread chunk after its known fields have been read, the load of the whole
file stops with a compatibility error. The record that triggered it has
already been added, the records after it are not read. (Confirmed.) A
read-only tool can be more lenient, but should report it.

Fewer than four bytes left at the end of a collection file are ignored.
(Confirmed.)

### Identity and time stamps

Every record starts with a 32-bit object id (`oid`) and a 32-bit update stamp
(`update_stamp`). Both are **stamps** as defined in 02: signed counts of whole
seconds since 2000-01-01 00:00:00 UTC.

A new `oid` is the current stamp if that is larger than the last id handed
out, otherwise the last id plus one. Ids are therefore unique and increasing
within one running application and are, approximately, the creation time.
While a vault is loaded every `oid` read raises that high-water mark, also
the `oid` of a record the loader then drops. (Confirmed.)

Within one collection file an `oid` is expected to be unique. If a loaded
record's `oid` is already present in the same collection, Banking4
silently gives the later record a fresh one, generated as just described.
(Confirmed for the account, group, filter and settings files.) A read-only tool
cannot reproduce that id and should report the duplicate instead. References
between records (`group_oid`, the per-account directory names) use these ids.

## Part 1: File map

### Paths

Everything lives in the container's root directory or exactly one directory
level below it. Banking4 resolves a path by cutting it at the first
directory separator; deeper nesting is never produced. Per-object directories
are named by a fixed prefix followed by the object's id as a signed decimal
number without padding, for example `Acct800000001`.

The separator exists only inside the application: it is the directory
separator of the platform the program runs on (a backslash on Windows, a
slash elsewhere), used to build a path text and to cut it apart again. It is
never stored. In the container, `Acct800000001` is a directory entry of the
root directory and `Stmt` is a file entry of that directory (01, section 7),
so a reader walks directory entries and never needs the character. The paths
in this document are written with `/` for readability. Entry names are
compared exactly, byte for byte and case-sensitively (01, section 7.3).

Directories are created on demand, so a directory or file that has never been
needed does not exist. A missing collection file is an empty collection.

Every collection file uses the binary record format. Banking4 also has an
XML mode for collections, but no collection uses it. Outbox payloads are
plain XML documents, not collections.

### Entries

"Read" says what `b4` does with the entry: **yes**, **no** (known, not
exported, skipped without a message), **later** (user data that is simply not
planned yet; until then treated like "no") or **never** (bank access data and
secrets: the content is not read at all, see below the table).

| Path pattern | Record type | Holds | Read | Layout |
|---|---|---|---|---|
| `AcctsNew` | account | All accounts. Authoritative, see [below](#accts-and-acctsnew). | yes | [Account](#account) |
| `Accts` | account | Legacy name of the account file. In a migrated vault it holds one placeholder record. | fallback only | [Account](#account) |
| `AcctGroups` | account group | Groups that accounts are sorted into. | yes | [Account group](#account-group) |
| `Acct<oid>/Stmt` | booking | Statement entries of one account. Only for kinds in the *statement* mask. | yes | 04, section 6 |
| `Acct<oid>/StmtMasterEntries` | booking | Legacy second booking file. Banking4 reads its records exactly as it reads those of `Stmt`, merges them into the statement collection on load and then deletes the file. | yes, as more bookings of the account, with a warning | 04, section 6 |
| `Acct<oid>/Paymts` | payment | Payments (transfers, direct debits) of the account. Only for kinds in the *payments* mask. | yes | 04, section 8 |
| `Acct<oid>/Stos` | standing order | Standing orders. Same mask as payments. | yes | 04, section 9 |
| `Acct<oid>/Portf` | portfolio item | Securities positions. Only for kind portfolio. | yes | 05, section 3.8 |
| `Acct<oid>/PortfTxns` | portfolio transaction | Securities transactions. Only for kind portfolio. | yes | 05, section 3.9 |
| `Acct<oid>/Assets` | asset | Items of an asset account. Only for kind asset. | yes | 05, section 3.11 |
| `Acct<oid>/AcctTaxExemptions` | tax exemption | Tax exemption orders. Any kind. | yes | 05, section 3.10 |
| `Acct<oid>/AcctFiles` | account document | Index of documents attached to the account (statements, bank mail). Each record contains a bank protocol token, which is dropped at decode time. | later | 05, section 3.12 |
| `Acct<oid>/<doc oid>.txt`, `.pdf`, `.bin` | raw bytes | Content of one account document. The extension follows the document type, see [below](#account-kinds-and-per-account-files). | later | 05, section 3.12.1 |
| `Acct<oid>/AcctPayChecks` | signature folder item ("pay check") | Only for kind bank. | no | 05, section 3.13 |
| `Acct<oid>/AcctPayCheckSplits` | signature folder split ("pay check split") | Only for kind bank. | no | 05, section 3.14 |
| `Payees` | payee | Address book of counterparties. | yes | 05, section 3.1 |
| `Payers` | payer | Counterparties for direct debits. | yes | 05, section 3.2 |
| `Mandates` | mandate | Direct debit mandates. | yes | 05, section 3.4 |
| `Invoices` | invoice | Invoices. | later | 05, section 3.15 |
| `Categories` | category | Category tree. | yes | 05, sections 3.3 and 4 |
| `ExchangeRatesBIN` | exchange rate | Exchange rates. | yes | 05, section 3.5 |
| `StockInstruments` | stock instrument | Securities master data. | yes | 05, section 3.6 |
| `Stock<oid>/StockPrices` | stock price | Price history of one instrument. | yes | 05, section 3.7 |
| `AcctStmtFilters` | booking filter | Saved booking filters (user interface state). | optional | [Booking filter](#booking-filter) |
| `ModelSettings` | setting | Per-vault key/value settings. **Contains secrets** (a crypto key and API tokens among preferences), see [below](#secrets-in-the-settings-file). | never | [Setting](#setting) |
| `Outbox` | outbox order | Orders queued for or sent to a bank. **Bank access data**: user id, TANs and the bank contact of each order. | never | 04, section 10 |
| `OutboxData/<order oid>.xml` | XML document | Payload of one outbox order. **Bank access data.** | never | not specified |
| `ImportExportPaymtsLog` | import/export log entry | Log of payment file imports and exports. | no | 05, section 3.17 |
| `FinPingBanks` | bank probe result | Developer feature. | no | not specified |
| `Avatars/<payee oid>` | raw bytes | Picture of a payee, image file content as is. No extension. | no | none (raw bytes) |
| `OnlineTraces` | online trace ("trace entry") | Index of bank communication traces (protocol logs). The trace content itself is kept outside the vault. | never | 05, section 3.16 |
| `OnlineContactKeys` | key entry | **Bank access data**: key pairs, user ids and passwords. | never | not specified, on purpose |
| `Contacts/*` | (not specified) | **Bank access data** (FinTS). | never | not specified, on purpose |
| `EbicsContacts/*` | (not specified) | **Bank access data** (EBICS). | never | not specified, on purpose |
| `ContactsHTML/*` | (not specified) | **Bank access data** (web access, PayPal). | never | not specified, on purpose |
| `SpoolerContacts/*` | (not specified) | **Bank access data** (file exchange folders). | never | not specified, on purpose |
| `Layout`, `TanLists`, `ExchangeRates`, `Traces` | unknown | Obsolete entries from old versions. Banking4 deletes them unread when it cleans up a vault. | no | unknown |

**Never read.** Nine entries of the root directory hold bank access data or
secrets: `Contacts`, `EbicsContacts`, `ContactsHTML`, `SpoolerContacts`,
`OnlineContactKeys`, `Outbox`, `OutboxData`, `ModelSettings` and
`OnlineTraces`. Their content must never be read, decoded or exported,
whether the entry is a file or a directory, and for a directory that covers
everything in it. A reader skips them by name before it touches a byte of
their content; it may report how many of them exist (the plaintext directory
gives that away anyway). The safe way to implement this is an
allow-list: read the content of an entry only if its name is one the reader
decodes, so that a name it has never heard of is not read either. The name
alone does not protect the bytes, though: before the reader decrypts
anything it reserves the clusters of every entry it does not read, so that
no entry with an allowed name can lead into them (01 §6.3, "fence off what
is not read"). For a directory among them that means walking its
unencrypted pages to find the clusters of what it lists, and reading none
of it. The first
five are out of scope for this project and their content is not specified
here. Record layouts exist in this set of documents for the outbox order
(04), the setting (below) and the online trace (05); they are documentation
of the format and are not implemented in `b4`.

**Dropped at decode time.** Some records that `b4` does decode contain single
fields that serve to log in at a bank or to authorise an order. A decoder
reads such a field only to advance past it and discards the value at once: it
is never kept in a memory structure, never written to the database and never
exported. They are:

| Record | Fields |
|---|---|
| account | `fints_customer_id`, `last_download_token`, `contact_guid`, `order_contact_guid`, `contact_name`, `order_contact_name`, `fints_account_sub_id` |
| payment (04, section 8.1) | `used_tan`, `order_reference_tan` |
| account document (05, section 3.12), once it is implemented | `acknowledge_token` |

**No host file system.** Some records name files: the online trace a trace
file on the computer's disk, the account document a file name for export. A
reader must never open, test or list anything in the host file system on
behalf of a name stored in a vault. Such a name is text, at most.

The "Layout" column says where the record layout is specified. Documents 04
and 05 use the names "signature folder item" and "signature folder split" for
what this document calls pay check and pay check split after the file names,
and "online trace" for the trace entry.

**Entries that are not in the table.** The table lists every name the Banking4
version described here uses. A vault written by another program version may
hold more. A reader should not fail on them and should not guess at their
content: skip the entry without reading it and report its name (for a file also
its size), once per name pattern. The same goes for an unknown file inside an
`Acct<oid>` or `Stock<oid>` directory and for a third level of directories,
which the Banking4 version described here never creates.

**Orphan directories.** A directory `Acct<oid>` or `Stock<oid>` whose `<oid>`
matches no account or stock instrument is not unknown: its files have known
names and layouts. `b4` decodes them like those of any other such directory
and reports the directory in a warning. The records keep the `<oid>` of the
directory as their owner. The booking adjustments that need the account
record (04, section 6.2, steps 3 and 4) have no account to ask. They are
applied as for an account without a currency of its own whose kind is
neither cash nor PayPal: step 3 takes its "no account currency" branch and
step 4 clears no date.

### Account kinds and per-account files

Whether a per-account file is ever loaded or created depends on the account
kind (see [account kind](#account-kind)):

| File | Account kinds |
|---|---|
| `Stmt` | bank, cash, credit card, PayPal, crypto (mask `0x67`) |
| `Paymts`, `Stos` | bank, credit card, PayPal (mask `0x25`) |
| `Portf`, `PortfTxns` | portfolio only (kind equals `0x08`) |
| `Assets` | asset only (kind equals `0x10`) |
| `AcctPayChecks`, `AcctPayCheckSplits` | bank (bit `0x01`) |
| `AcctFiles` | any real kind (mask `0x7F`) |
| `AcctTaxExemptions` | any |

A reader should not rely on this: read whichever files exist.

The content of an account document is stored next to the index, in a file
named after the document record's `oid` with an extension chosen by the
document type:

| Document type | Meaning | Extension |
|---|---|---|
| 1 | statement, SWIFT MT940 text | `.txt` |
| 3 | statement, PDF | `.pdf` |
| 5 | bank document, PDF | `.pdf` |
| 0, 2, 4, 6, 7, 8, 9, 10 | unknown; statement in ISO 8583 form; user file; empty; bank SWIFT message; mail; mail as PDF; statement camt.053 | `.bin` |

The document type is a field of the document record, which is specified in
05, section 3.12 (the enumeration is 05, section 6.15). Note that "mail as
PDF" is stored with the `.bin` extension.

Removing an account deletes its `Acct<oid>` entry. When Banking4 cleans up a
vault it also deletes root entries whose name contains `Acct` followed by
digits when those digits are not the `oid` of an account, and files in an
account directory whose name contains `<digits>.<lowercase letters>` and
equals no document record's file name. Both tests are substring matches, not
anchored.

### `Accts` and `AcctsNew`

Both names hold the same record type in the same layout. The difference is
only the name. Banking4 loads them like this:

1. If `AcctsNew` exists, load it. `Accts` is not looked at.
2. Otherwise, if `Accts` exists: load it, save the result as `AcctsNew`,
   delete `Accts`, and write a new `Accts` that contains exactly one freshly
   created **placeholder account** of kind none (0).
3. Otherwise the vault has no accounts; the collection will later be saved as
   `AcctsNew`.

From then on every save goes to `AcctsNew`. The placeholder file is written
once during migration and never updated. A vault created by a current version
never gets an `Accts` file at all.

When reading an account file the loader drops every record whose kind is 0,
so the placeholder can never become a real account. (Confirmed for the drop.)

What follows for a reader:

- `AcctsNew` is authoritative whenever it exists.
- If only `Accts` exists the vault was last written by a version from before
  the rename, and `Accts` holds the real accounts.
- If both exist, `Accts` is the placeholder (or stale) and must be ignored.
- Skip records of kind 0 in either file. The kind is the low eight bits of
  the stored value.

The first and third point are confirmed: Banking4 reads nothing from `Accts`
when `AcctsNew` exists. The demo vault (`tests/fixtures/vault/`), a test file
produced with Banking4, has both: an `AcctsNew` with six accounts and an
`Accts` with the single kind-0 placeholder as Banking4 creates it.

An old program version that only knows `Accts` therefore sees a migrated vault
as containing a single account of kind none: no kind bits, the account number
set to the decimal form of its own `oid`, default country and currency, default
flags, nothing else. Why Banking4 uses a rename with a decoy instead of the
ordinary compatibility mechanism is not known, see open
questions.

### Other renamed or retired entries

No other collection has a live old/new pair. What exists are leftovers:

- `ExchangeRates` was replaced by `ExchangeRatesBIN`. The old file is not
  migrated, only deleted when Banking4 cleans up a vault.
- `Traces` was replaced by `OnlineTraces`; same treatment.
- `Layout` and `TanLists` are deleted in the same clean-up.
- `Acct<oid>/StmtMasterEntries` is a legacy second booking file. After `Stmt`
  has been loaded, its records are appended to the statement collection and
  the file is deleted. Banking4 reads its records exactly as it reads the
  records of `Stmt` (04, section 6), so a reader that finds the file treats
  its records as further bookings of the account.
- The payee file doubles as a migration source: payees that still carry a
  mandate are turned into payer records when the payee collection is first
  loaded. This changes the in-memory model and, on the next save, the files.
  It is application behaviour, not a different layout.

### What is loaded when

All collections are loaded on first access and cached. When a vault is opened
normally, Banking4 loads these at start-up, in this order:

1. `ModelSettings` (needed to set up the secret store, see below)
2. `AcctGroups`
3. `AcctsNew` / `Accts`
4. bank access data
5. `OnlineTraces`
6. `ExchangeRatesBIN`
7. `Categories`
8. `Payees` (which then also loads `Payers`)
9. `Mandates`
10. `Invoices`
11. `Outbox`
12. for every account: `Stmt`, `Paymts`, `Stos`, `Assets`, `AcctPayChecks`,
    `AcctPayCheckSplits`, `Portf`, `AcctFiles`, `AcctTaxExemptions`

Not loaded by the start-up itself, so loaded lazily when first used:
`AcctStmtFilters`, `StockInstruments`, every `Stock<oid>/StockPrices`,
`Acct<oid>/PortfTxns`, `ImportExportPaymtsLog`, `FinPingBanks`,
`OnlineContactKeys`, payee pictures, outbox payloads and document contents.
Some of them may still be pulled in indirectly by the eager loads; whether
that happens is not known.

Several collections are switched off entirely by feature switches of branded
editions (groups, payees, payers, mandates, invoices, categories,
outbox, payments, standing orders, documents, pay checks, tax exemptions,
portfolio transactions). A vault from such an edition simply lacks the files.

The ordinary product has such switches too. Its default product variant,
the one in effect when the application's manifest sets no parameter for it,
does not load portfolio transactions, payers, batches and invoices;
Banking4 has these lists only when that parameter selects the professional
or the business edition. (The demo vault in `tests/fixtures/vault/` was
therefore written with the professional edition switched on.) None of this
changes a layout: the files are the same whichever variant wrote them, and a
reader decodes whichever exist.

After a collection has been loaded it is sorted, and it is saved in sorted
order. Accounts and groups sort by `position`, then `oid`; the orders of the
other lists are given with their records (04, section 6.4; 05, sections 3.1,
3.4, 3.5, 3.8 and 4.1). A reader should sort itself rather than rely on file
order.

`ModelSettings` is also read when another list is asked for: loading the
exchange rates makes Banking4 read its settings on its own. If the settings
file is not a valid collection, Banking4 reports the load of it as failed
and carries on. (Confirmed.) A reader has no reason to imitate this; `b4`
reads the exchange rates and never the settings.

### Secrets in the settings file

`ModelSettings` is a flat list of name/value strings. Most entries are
harmless preferences, but the application also keeps secrets there. The
known ones:

| Key | Content |
|---|---|
| `Model_CryptorKeyBase64` | Base64 key of the vault's internal secret store. That store is used only to save and fetch **stored online banking PINs**. |
| `SevDesk_ApiToken` | API token of a bookkeeping service. |
| `PrepaidLoadOrderPanel_ProviderToken` | Token of a prepaid top-up provider. |

The settings collection is exposed to the rest of the application as a
general settings store, so any component can add keys and this list is not
exhaustive. For that reason `b4` does not read `ModelSettings` at all: it is
one of the nine entries that are never read (see [Entries](#entries)), and no
allow-list of harmless keys is attempted.

Other keys used by the part of Banking4 that manages the vault's data, none
of them secret:

- `Outbox_StoragePeriod`: a retention period in the text form described
  below.
- Keys starting with `AcctStmtEntryFlag_`: user-defined captions of the
  booking colour flags.
- Preferences stored as text: `PrintingTextSizeFixed`, `PrintingTextSize`,
  `PrintingTableLandscape`, `PrintingFormsLandscape`, `DoNotAutoAddAccts`,
  `DoNotAutoAddPayees`, `DoNotAutoVopPayees`, `DoNotAutoAddStos`,
  `DoNotAutoDetectCrossEntries`, `DoNotAutoImportSepaFiles`,
  `AutoRundrufOnStart`, `Totals_NetAmounts`, `CreditBatchBookingPreference`,
  `DebitBatchBookingPreference`.
- `PrepaidLoadOrderPanel_Category` and `PrepaidLoadOrderPanel_PhoneNumber`:
  last input of a top-up form. The second is personal data.

When a key is missing from the vault the application falls back to the
per-installation settings outside the vault, so absence of a key says nothing.

### Format and version markers

There is no vault-wide data format or schema version in `ModelSettings` or
in any other collection. Banking4 never reads such a marker. The only
versioning devices are:

- the short-read rule and chunk structure described above;
- per-record version bytes inside some record types (the account has one, see
  below);
- renamed files (`AcctsNew`, `ExchangeRatesBIN`, `OnlineTraces`).

The container header has its own magic and creation stamp (see the container
document); the vault's creation date shown by the application is that stamp,
interpreted with the same 2000-01-01 epoch as ids.

## Part 2: Record layouts

Column "Chunk" is the chunk the read happens in. "Writer" notes are given
where the writer does something other than write the field it later reads.
Unless a difference is called out, the writer emits exactly the sequence the
reader consumes; this is confirmed field by field for every record in this
part.

### Record base

Common header of **every** record. Every record layout below starts with
these two fields.

| # | Chunk | Primitive | Field | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id: creation time in seconds since 2000-01-01 UTC, made unique as described above. | Raises the id high-water mark. |
| 2 | 1 | `i32` | `update_stamp` | Last modification, same unit. | In memory `-1` means "not stamped yet"; the writer replaces it with the current time, so `-1` should not appear on disk. Any other value, negative ones included, is written as it is. |

No conditionals, no further processing. Both values are plain signed 32-bit
numbers to the reader: zero, negative values and the two extremes are read
back unchanged. In-memory bookkeeping (row number,
dirty flags, owning collection) is not stored.

### Monetary base and account-choice base

In Banking4 the account and the account group build on two intermediate base
types: a "record with an amount and a currency" and an "entry of the account
chooser". **Neither stores anything.** They add no field of their own, when
a record is read or when it is written. So an account or group record is the
two base fields immediately followed by its own fields.

The account-choice base distinguishes four kinds of chooser entries; only two
are ever stored:

| Value | Name | Stored |
|---|---|---|
| 0 | `none` | |
| 1 | `account` | yes, in the account file |
| 2 | `account_set` | no, built at run time |
| 3 | `account_group` | yes, in `AcctGroups` |
| 4 | `all_accounts` | no, built at run time |

This value is not on the wire; the file determines the type.

### Account

Stored in `AcctsNew` (or `Accts`). The record base comes first (fields 1 and
2), then:

| # | Chunk | Primitive | Field | Meaning | Notes |
|---|---|---|---|---|---|
| 3 | 1 | `i32` | `account_kind` | [Account kind](#account-kind). | Only the low 8 bits are kept. |
| 4 | 1 | `code(2)` | `country_code` | ISO country of the bank. | |
| 5 | 1 | `string` | `bank_code` | National bank code. Non-bank kinds get a fixed pseudo code when created, see below. | |
| 6 | 1 | `string` | `account_number` | National account number. | |
| 7 | 1 | `code(3)` | `currency` | ISO currency code. | |
| 8 | 1 | `string` | `name` | Display name. | |
| 9 | 1 | `string` | `contact_name` | Name of the bank access used for downloads. | Cleared for cash accounts. |
| 10 | 1 | `string` | `iban` | | |
| 11 | 1 | `string` | `bic` | | |
| 12 | 1 | `string` | `holder_name` | Account holder. | |
| 13 | 1 | `string` | `description` | Free text. | |
| 14 | 1 | `date_clamped` | `last_download_date` | Day of the last statement download. | Superseded by field 53 when that is set. |
| 15 | 1 | `bool` | (none) | Unused. | Discarded. Writer: false. |
| 16 | 1 | `i32` | `statement_retention_days` | Retention period for bookings, as a number of days. | Converted with the [day form](#day-form) rule. Writer: the period's minimum length in days. Lossy, see there. The purge date of this period is derived right here, before the version test. |
| 17 | 1 | `u8` | `record_version` | Version of this record. | Not kept. Writer of the Banking4 version described here: 26. Version 27 has been seen, see below. |
| 18 | 1 | `i32` | `sync_flags` | [Sync flags](#sync-flags): what a send/receive run fetches for this account. | |
| 19 | 1 | `date_clamped` | `last_sync_date` | Day of the last send/receive. | Superseded by field 38. |
| 20 | 1 | `date_clamped` | `statement_balance_date` | Day of the booked balance. | Superseded by field 51. |
| 21 | 1 | `decimal` | `statement_balance` | Booked balance according to the statements. | |
| | | | | **If `record_version` < 2 the reader stops here.** | None of the processing below happens either, and the reader does not advance to the second chunk. |
| 22–25 | 1 | 4 × `bool` | (none) | Unused. | Discarded. Writer: false. |
| 26 | 1 | `i32` | (none) | Unused. | Discarded. Writer: 0. |
| 27 | 1 | `date_clamped` | `online_balance_date` | Day of the balance reported online. | Superseded by field 52. |
| 28 | 1 | `decimal` | `online_balance` | Balance reported by the bank. | |
| 29 | 1 | `string` | `order_contact_name` | Name of the bank access used for sending orders. | |
| 30 | 1 | `i32` | `flags` | [Account flags](#account-flags). | |
| 31 | 1 | `date_clamped` | `opening_date` | Date the account was opened. | |
| 32 | 1 | `string` | `holder_address` | Holder's street and town in one string. | Split at the first line feed: the part before it is `holder_street`, the rest `holder_town`, each trimmed, empty becomes null. Without a line feed the whole string is the street, unchanged, and the town is null. Writer: null if both parts are null, else street + line feed + town. |
| 33 | 1 | `decimal` | `credit_interest_rate` | | |
| 34 | 1 | `decimal` | `debit_interest_rate` | | |
| 35 | 1 | `decimal` | `overdraft_interest_rate` | | |
| 36 | 1 | `decimal` | `overdraft_limit` | | |
| 37 | 1 | `string` | `fints_account_sub_id` | Sub-account discriminator used in FinTS dialogs. | Cleared in two cases, see processing. |
| 38 | 1 | `ticks` | `last_sync_time` | Time of the last send/receive. | If set, replaces the value of field 19. Writer emits fields 19 and 38 from the same value. |
| 39 | 1 | `string` | `creditor_id` | SEPA creditor identifier. | |
| 40 | 1 | `string` | `reference_account_iban` | IBAN of a linked reference account. | |
| 41 | 1 | `bool` | `has_internal_transfer_list` | Whether the next two fields are present. | |
| 42 | 1 | `i32` | `internal_transfer_count` | Number of entries. | **Only if field 41 is true.** A negative count makes Banking4 fail on the record. Banking4 does not compare the count with the bytes that are left; a reader must, see [below](#short-long-and-damaged-records). |
| 43 | 1 | `string` × count | `internal_transfer_ibans` | IBANs the bank reported as permitted targets for internal transfers (rebooking between own accounts) from this account. Filled by an online banking download, used when such a transfer is entered. | **Only if field 41 is true.** Entries may be null (an empty entry is written like a null one). An empty list (count 0) is distinct from "absent". |
| 44 | 1 | `string` | `originator_name` | Name to use as originator of payments. | |
| 45 | 1 | `string` | `last_download_token` | Opaque continuation token of the last download. | |
| 46 | 1 | `string` | `reserved_text` | No known use. | |
| 47 | 1 | `u16` | `position` | Sort position among the accounts. | Unsigned: a negative value written by Banking4 reads back as 65535. |
| 48 | 1 | `i32` | `group_oid` | `oid` of the account group, 0 for none. | |
| 49 | 1 | `u16` | `balance_flags` | [Balance flags](#balance-flags). | |
| 50 | 1 | `decimal` | `available_funds` | Available amount as reported or computed. | |
| 51 | 1 | `ticks` | `statement_balance_time` | | If set, replaces field 20. |
| 52 | 1 | `ticks` | `online_balance_time` | | If set, replaces field 27. |
| 53 | 1 | `ticks` | `last_download_time` | | If set, replaces field 14. |
| 54 | 1 | `u16` | `notify_flags` | [Notification flags](#notification-flags). | |
| 55 | 1 | `decimal` | `notify_min_debit` | Notify about debits from this amount. | |
| 56 | 1 | `decimal` | `notify_min_credit` | Notify about credits from this amount. | |
| 57 | 1 | `decimal` | `notify_min_balance` | Notify when the balance falls below. | |
| 58 | 1 | `decimal` | `notify_max_balance` | Notify when the balance rises above. | |
| 59 | 1 | `decimal` | `notify_spending_limit` | Spending limit for notifications. | |
| 60 | 1 | `string` | `fints_customer_id` | Customer id used in FinTS dialogs. | A login identifier; dropped at decode time, see below the table. Cleared for cash accounts. |
| 61 | 1 | `string` | `payments_retention` | Retention period for payments, [text form](#text-form). | A text that does not parse makes Banking4 fail on the record, see there. Its purge date is derived right after the read. |
| 62 | 1 | `string` | `files_retention` | Retention period for documents, text form. | Same. |
| 63 | 1 | `ticks` | (none) | Unused. | Discarded. Writer: "no time". |
| 64 | 1 | `guid` | `contact_guid` | Identifies the bank access used for downloads. | |
| 65 | 1 | `guid` | `order_contact_guid` | Identifies the bank access used for orders. | |
| 66 | 1 | `date_strict` | `sevdesk_last_entry_date` | Bookkeeping-service sync position. | Not clamped, unlike the other dates of this record. |
| 67 | 1 | `i32` | `sevdesk_flags` | [Bookkeeping-service flags](#bookkeeping-service-flags). | |
| 68 | 1 | `string` | `icon_name` | Name of a user-chosen icon. | |
| | | | | **Advance to the next chunk.** Unconditional; the result is ignored. If there is no second chunk the cursor stays where it is. | Writer: always starts a second chunk here. |
| 69 | 2 | `u8` | (none) | Unused. | Discarded. Writer: 1. |

**Bank access fields.** Seven fields of this table identify or serve an
online banking login: `contact_name` (9), `order_contact_name` (29),
`fints_account_sub_id` (37), `last_download_token` (45), `fints_customer_id`
(60), `contact_guid` (64) and `order_contact_guid` (65). `b4` reads them to
stay in step and drops them at decode time: they are not kept in memory and
are not exported (see [Entries](#entries)). The table documents them because
a reader has to know their width, and the processing steps below mention
them because that is what Banking4 does.

There are no "more chunks" tests and, apart from the stop at version < 2, no
version-dependent read paths. Older and shorter records are handled purely by
the short-read rule. (Confirmed, see
[Short, long and damaged records](#short-long-and-damaged-records).)

Fields 14, 19, 20 and 27 are the old day-precision form of four time stamps;
fields 53, 38, 51 and 52 are their newer tick-precision form. The writer
always writes both from the same in-memory value. The reader takes the
tick-precision value whenever it is later than "no time", otherwise the day;
the two are not compared with each other, so a tick value of a single tick
replaces any day. (Confirmed.)

#### Processing after the read

Applied in this order, only when `record_version` ≥ 2:

1. If the kind has none of the bits bank, cash, credit card, PayPal, crypto
   (mask `0x67`): `statement_balance` becomes 0 and its time "no time".
2. If the kind is exactly cash (`0x02`): `fints_account_sub_id`,
   `fints_customer_id`, `last_download_token`, `contact_name`,
   `order_contact_name` and the internal transfer list become null;
   `online_balance` becomes 0; the online balance time, last download time and
   last sync time become "no time"; `sync_flags` becomes 0; both contact GUIDs
   become all zero.
3. If `record_version` < 19 and the kind is exactly bank (`0x01`): the bits
   *account info* and *payees* (`0x0500`) are added to `sync_flags`.
4. If the kind is exactly cash and `currency` names a currency that the
   application's currency table classifies as crypto: the kind becomes crypto
   (`0x40`). In the Banking4 version described here those codes are `ETH`,
   `LTC`, `XBT` and `XRP`. The code must match a primary code of the table
   exactly: lower case (`xbt`) does not match, and neither does `BTC`, which
   the table knows only as an alternate code of `XBT`. Everything step 2
   cleared stays cleared.
5. If `bank_code` is exactly eight characters, all of them ASCII digits `0`
   to `9`, and starts with `120300`: `fints_account_sub_id` becomes null.
   Other digit characters (full-width or Arabic-Indic digits, superscripts)
   do not count as digits, and surrounding blanks make the code longer than
   eight characters.

"Exactly" in steps 2 to 4 refers to the kind as kept, that is to the low
eight bits of field 3: a stored kind of `0x102` is a cash account, `0x03` is
neither bank nor cash. All five steps are confirmed.

The 0 that steps 1 and 2 put in place of a balance is a plain zero with scale
0 (`0`), not a zero with the scale of the amount it replaces (`0.00`). The
difference shows only where the scale of a decimal is kept.

The split of `holder_address` trims each part with the string trimming of
the runtime Banking4 runs on: the white space of `code(n)` in 02 (`09`..`0D`,
`20`, `85`, `A0`) and the other Unicode space, line and paragraph separators.
The control characters `1C`..`1F` are not trimmed. A carriage return is not
a line break here: `a`, CR, LF, `b` gives street `a` (the CR is trimmed
away) and town `b`, while `a`, CR, `b` without a line feed stays one unsplit
street.

Derived, never stored: a purge date for each of the three retention periods,
computed from the period and today's date while the record is read (see
[Retention period](#retention-period) for the rule and for the one way this
can make the read fail).

For records with `record_version` < 2 the two text-form retention periods are
never assigned and stay "not set" (unit days, count 0) rather than
"unlimited". (Confirmed.)

#### Short, long and damaged records

How Banking4 reads a record that is shorter or longer than the layout, or
damaged. (All of this is confirmed.)

- **A chunk that ends early** gives defaults for everything behind the cut,
  exactly by the rules of 02 section 3, including the special paths (a string
  whose length reaches past the cut poisons the rest of its chunk; the next
  chunk is read normally).
- **A record cut before field 17** has `record_version` 0 and therefore takes
  the early exit: no fields behind field 21, no processing. If such a record
  still has its second chunk, that chunk stays unread and Banking4
  stops the load with the compatibility error.
- **A record without the second chunk** (an old single-chunk record, or one
  that has lost its second chunk) reads the same as with it. The advance is a
  no-op and field 69 is read from whatever follows `icon_name` in the first
  chunk, or defaults to 0. Nothing depends on it.
- **Unknown bytes at the end of either chunk** are ignored, when a single
  record is read and when a whole file is loaded. So are bytes between the
  last chunk and the end of the record (02 section 2.4).
- **An unknown third chunk** is ignored when the record is read: all known
  fields come back. The loader keeps that record and then stops with the
  compatibility error, as described under
  [Records, chunks and tolerance](#records-chunks-and-tolerance).
- **Counts are trusted.** With field 41 true Banking4 allocates a list of
  `internal_transfer_count` entries before reading any of them. A count
  larger than the strings actually present yields a list of that length
  whose remaining entries are null (once the chunk is exhausted) or are
  taken from the bytes of the following fields, which then decode as
  garbage or fail. A negative count fails at once. A reader must not follow
  Banking4 here: never allocate by the count; refuse a negative count, and
  a count larger than the bytes left in the chunk divided by 2, the smallest
  size of one element (a `string` takes at least two bytes); make that
  comparison as a division or in checked arithmetic, never as a 32-bit
  product. A refused count makes the record undecodable; it is reported and
  the next record is read (README, "Coping with other versions").
- **What makes Banking4 fail** on an account record, besides the
  general cases of 02 section 7 (an invalid `decimal`; a `ticks` value out of
  range in fields 38, 51, 52, 53 and also in the unused field 63; an
  impossible date in the one `date_strict` field 66): a negative
  `internal_transfer_count`; a `payments_retention` or `files_retention`
  text that does not parse; and such a text whose purge date would fall
  before the year 1. These are ordinary runtime errors raised in the middle
  of the read; the record is not added and the load of the file stops there.
- The five `date_clamped` fields (14, 19, 20, 27 and 31) never fail:
  impossible dates are clamped as described in 02 (month 13 becomes
  December, 31 February the last day of February, a year above 9999 becomes
  9999, any zero part means "no date").

#### Reader/writer differences

The writer emits the same sequence of primitives. The asymmetries, all
intended by the format:

- Unused fields are written as constants and ignored on read (15, 22–26, 63,
  69).
- `statement_retention_days` round-trips only for the preset periods.
- `holder_address` is joined on write and split on read; white space around
  the two parts and empty parts are not preserved.
- Empty strings come back as null (property of `string`).
- `code(n)` fields are cut to n characters, and characters above U+00FF lose
  their high byte.
- `position`, `balance_flags` and `notify_flags` are written from 32-bit
  values but only their low 16 bits are stored.
- `account_kind` is written as a 32-bit value from an 8-bit one, so the
  writer never sets the upper 24 bits the reader ignores.
- The time of day of `opening_date` and `sevdesk_last_entry_date` is not
  stored.
- The two text-form periods come back with unit days when they were
  unlimited or had a unit outside the three defined ones. The day-form
  period comes back with unit days also when it was "not set".
- Steps 1 to 5 above change values on read, so reading and re-saving an old
  record is not byte-identical.

#### Record version 27

Account files written by a newer program version than the one described here
carry `record_version` 27 and **ten additional bytes at the end of chunk 1**,
after `icon_name`. The Banking4 version described here skips them. In the one
real vault examined they were zero in every one of its 21 account records.
Their layout is unknown. Chunk 2 is unchanged (one byte, value 1). The known
fields of these records decode as described here: all 21 records equal the data
exported from Banking4 value by value (see Verification).

#### Pseudo bank codes

Set when an account is created, not enforced on read:

| Kind | `bank_code` |
|---|---|
| cash | `00001000` |
| PayPal | `01005000` |
| crypto | `02000000` |
| asset | `03000000` |
| portfolio | `04000000` |
| credit card | `99000000` |

A new account's `account_number` is initialised to the decimal form of its
`oid`.

### Account group

Stored in `AcctGroups`. Record base first, then:

| # | Chunk | Primitive | Field | Meaning | Notes |
|---|---|---|---|---|---|
| 3 | 1 | `u16` | `position` | Sort position among the groups. | Unsigned, as for accounts. |
| 4 | 1 | `string` | `name` | Display name. | |
| 5 | 1 | `string` | `description` | Free text. | |
| 6 | 1 | `i32` | `flags` | [Group flags](#group-flags). | |

One chunk, no conditionals, no version byte, no processing. The writer emits
the same four fields. (Confirmed, including flag values with undefined bits,
which are kept as they are.) Which groups are shown collapsed is kept in the
per-installation settings, outside the vault. Membership is stored on the account (`group_oid`), not here.

### Booking filter

Stored in `AcctStmtFilters`. Record base first, then:

| # | Chunk | Primitive | Field | Meaning | Notes |
|---|---|---|---|---|---|
| 3 | 1 | `string` | `name` | Name of the saved filter. | |
| 4 | 1 | `u8` | `period` | [Filter period](#filter-period). | Not range-checked: any byte value is kept. |
| 5 | 1 | `ticks` | `min_date` | Start of an explicit date range. | Sanitised: a value on or after 9999-12-31 00:00 becomes "no time"; any other set value is cut to midnight (so a time of day on 0001-01-01 also ends as "no time"). |
| 6 | 1 | `ticks` | `max_date` | End of an explicit date range. | Same. |
| 7 | 1 | `u8` | `amount_filter` | [Filter amount mode](#filter-amount-mode). | |
| 8 | 1 | `decimal` | `min_amount` | | |
| 9 | 1 | `decimal` | `max_amount` | | |
| 10 | 1 | `string` | `search_text` | Free text to search for. | |
| 11 | 1 | `i32` | `flag_mask` | Bit n set selects bookings carrying [booking colour flag](#booking-colour-flag) n (n = 0..7). | |
| 12 | 1 | `i32` | `category_count` | Number of category entries. | Values ≤ 0 mean no list. Writer: 0 for no list and for an empty list, which therefore comes back as no list. Banking4 trusts a positive count like the account's list count. A reader must not: never allocate by the count; refuse a count larger than the bytes left in the chunk divided by 2, the smallest size of one element; compare by division or in checked arithmetic. |
| 13 | 1 | `string` × count | `categories` | Categories to match. | A stored string that is exactly one space stands for the empty string (which `string` cannot represent) and is turned back on read; two spaces stay two spaces. Writer does the reverse for an empty entry; a null entry is written as null and stays null. The whole list is dropped in editions without categories. |
| 14 | 1 | `u8` | `transaction_kind` | [Transaction kind](#transaction-kind), 0 meaning "do not filter". | Not range-checked. |
| 15 | 1 | `bool` | `unread_only` | Only unread bookings. | |
| 16 | 1 | `bool` | (none) | Unused. | Discarded. Writer: false. |

One chunk, no version byte. After the read the reader derives a
`search_amount` by handing `search_text` to the application's amount parser
and taking the absolute value; when the text is not an amount it is a
sentinel, the smallest decimal (−79228162514264337593543950335). It is not
stored, a value held in memory is not written, and a reader of the file does
not need it. The parser follows the application's amount format (with the
German format `12,34` and `1.234,56` are amounts and `12.34` is 1234); its
rules are not part of the file format and are not specified here. Whether a
filter is one of the built-in presets is not stored either: the marker is
false after every read; presets are generated at run time and are not in the
file.

Reader/writer differences: the writer stores `min_date` and `max_date`
unmodified, the reader sanitises them. `period`, `amount_filter` and
`transaction_kind` are written from 32-bit values of which only the low byte
is stored. An empty category list comes back as no list.

What makes Banking4 fail on a filter record: a `ticks` value out
of range in field 5 or 6, an invalid `decimal`. A negative `category_count`
is harmless here, unlike in the account. (All of this section is confirmed.)

### Setting

Stored in `ModelSettings`. Record base first, then:

| # | Chunk | Primitive | Field | Meaning | Notes |
|---|---|---|---|---|---|
| 3 | 1 | `string` | `name` | Key. | Records without a name are loaded but unreachable. |
| 4 | 1 | `string` | `value` | Value, always text. | An empty value reads back as null. Setting a value to null removes the record. |

One chunk, nothing else. (Confirmed for the record; the loader keeps every
record, also one without a name and one that repeats a name.) If a name
occurs twice, the later record wins when a value is looked up; that lookup
is not confirmed. Booleans and numbers are stored in their text
form by whoever owns the key.

### Retention period

Not a record but a value type used by the account (three times) and by the
`Outbox_StoragePeriod` setting. It is a pair (`unit`, `count`):

| `unit` | Name |
|---|---|
| 0 | `days` |
| 1 | `calendar_months` |
| 2 | `calendar_years` |

`count` 0 means "not set". `count` 2147483647 means "unlimited". It has two
wire forms. They differ in what survives of the unit: the day form always
reads back with unit `days` for "not set" and for "unlimited"; the text form
gives unit `days` for "unlimited" but keeps the unit of a "not set" period
(`M0` is zero months).

#### Text form

A `string`:

- null or empty: unlimited;
- otherwise the first character selects the unit (`D` days, `M` calendar
  months, `Y` calendar years, anything else days; the letters are case
  sensitive, so `m5` is five days) and the rest is the count, parsed as a
  signed 32-bit integer in decimal.

The count is parsed by the integer parser of the runtime Banking4 runs on,
which accepts ASCII digits with an optional leading `+` or `-`, optional
white space (`09`..`0D`, `20`) before and after, and trailing NUL
characters. So `D 5`, `D+5` and
`D05` are five days and `D-1` is a count of −1. Everything else makes
Banking4 **fail** with an ordinary runtime error: no count at all
(`D`, or a single character such as `5`, which is taken as the unit; `55`
is five days), a unit letter after white space (` D5`), any other character
(`D5x`, `D1,5`, `D5.0`), non-ASCII digits, and a number outside the 32-bit
range. The count is not range-checked otherwise: negative counts and counts
of 36500 or more are kept as read. (The parser uses the current culture,
which decides the sign characters; what is said here holds for a German
culture, see open question 13.)

The writer produces null for unlimited, whatever the unit; else the unit
letter followed by the count. "Not set" is therefore written with its unit
and a zero count: `D0`, `M0` or `Y0`. A unit outside the table is written as
`?` and so reads back as days. (Confirmed.)

#### Day form

An `i32` number of days, used only for the account's booking retention:

- 0: not set;
- otherwise the first of these presets whose length in days is at least the
  stored number: 1 month (30), 3 months (90), 6 months (180), 1 year (365),
  2 years (730), 6 years (2190), 10 years (3650); if none is, unlimited.
  Every negative number therefore gives 1 month.

The writer stores the period's minimum length in days: 2147483647 when the
count is 36500 or more or the unit is none of the three; otherwise the count
for days, 30 × count for months, 365 × count for years (32-bit arithmetic,
negative counts included). A period that is not one of the presets is
therefore changed by a save and load, for example 45 days becomes 3 months,
31 days becomes 3 months, 3651 days becomes unlimited and −1 month becomes
1 month. (Confirmed, reader and writer.)

The purge date derived from a period is: none if the count is ≤ 0 or ≥ 36500;
for days, today minus count days; for calendar months, the first day of the
current month minus count months; for calendar years, January 1st of the
current year minus count years. "Today" is the local date of the machine.
(Confirmed.)

Banking4 computes the three purge dates while it reads an account record. If
the date would fall before 0001-01-01 the date arithmetic fails and with it the
read of the record. That needs a count of at least the current year number
in years, or more than about 24300 in months; the text form can express
such periods (`Y3000`), the day form and the application's own choices
cannot. A reader that does not purge anything has no reason to compute
these dates and should not fail on such a text.

## Enumerations

Values are given in hexadecimal for bit sets and in decimal otherwise.
"Aliases" are additional named values in Banking4 that are
combinations of the single bits; they are masks Banking4 uses, not
separate states.

### Account kind

Bit set stored in 8 bits (read from an `i32`). A stored account has exactly
one bit; 0 marks the placeholder.

| Value | Name |
|---|---|
| `0x00` | `none` |
| `0x01` | `bank` |
| `0x02` | `cash` |
| `0x04` | `credit_card` |
| `0x08` | `portfolio` |
| `0x10` | `asset` |
| `0x20` | `paypal` |
| `0x40` | `crypto` |

Aliases (masks): all kinds `0x7F`; has statements `0x67`; has payments `0x25`;
has standing orders `0x25`; has documents `0x7F`; has pay checks `0x01`;
supports online banking `0x3D`; supports FinTS `0x1D`; supports EBICS `0x01`;
has interest rates `0x05`; has a bank code `0x1D`; has a country `0x1D`; has
an account holder `0x1D`; has an IBAN `0x15`; shows summary content `0x18`.

### Account flags

Bit set, 32 bits.

| Value | Name | Meaning |
|---|---|---|
| `0x00000001` | `hidden` | Account is hidden. |
| `0x00000002` | `exclude_from_totals` | |
| `0x00000004` | `legacy_statement_after_order` | Not supported any more. |
| `0x00000008` | `show_holder_name` | |
| `0x00000010` | `nagging_contact` | Remind about the download bank access. |
| `0x00000020` | `nagging_order_contact` | Remind about the order bank access. |
| `0x00000040` | `exclude_from_general_sync` | Not part of "fetch everything". |
| `0x00000080` | `exclude_from_category_totals` | |
| `0x00000100` | `statement_schedule_weekly` | Part of a 3-bit field, see below. |
| `0x00000200` | `statement_schedule_monthly` | Part of a 3-bit field, see below. |
| `0x00000800` | `show_summary_items` | |
| `0x00001000` | `do_not_acknowledge_statements` | |
| `0x00002000` | `vop_opt_in_batch_transfers` | Payee verification for batches. |
| `0x00004000` | `vop_opt_out_single_transfers` | Payee verification for single transfers. |
| `0x01000000` | `legacy_include_in_replenish` | No longer supported. |

Bits `0x00000700` form a small field, the statement schedule: 0 always,
`0x100` weekly, `0x200` monthly. Aliases: the mask `0x700` and the value 0
("always") have names of their own. A newly created account starts with
`0x1200` (monthly, do not acknowledge statements).

### Group flags

Bit set, 32 bits.

| Value | Name |
|---|---|
| `0x1` | `show_first_account_icon` |
| `0x2` | `anonymous` |
| `0x4` | `exclude_from_totals` |

### Balance flags

Bit set, defined on 32 bits but stored in 16.

| Value | Name |
|---|---|
| `0x0001` | `has_bank_overdraft_limit` |
| `0x0002` | `has_bank_available_funds` |
| `0x0100` | `show_overdraft_limit` |
| `0x0200` | `show_available_funds` |
| `0x0400` | `show_credit_interest_rate` |

### Notification flags

Bit set, defined on 32 bits but stored in 16.

| Value | Name |
|---|---|
| `0x1` | `disable_debit_credit_notifications` |
| `0x2` | `enable_balance_notifications` |
| `0x4` | `enable_spending_limit_notifications` (specific to one branded edition) |

### Sync flags

Bit set, 32 bits. Says which kinds of data a send/receive run handles for the
account.

| Value | Name |
|---|---|
| `0x00000002` | `balances` |
| `0x00000004` | `statements` |
| `0x00000008` | `standing_orders` |
| `0x00000010` | `postdated_transfers` |
| `0x00000020` | `portfolio` |
| `0x00000040` | `assets` |
| `0x00000080` | `files` |
| `0x00000100` | `account_info` |
| `0x00000200` | `pay_checks` |
| `0x00000400` | `payees` |
| `0x00000800` | `portfolio_statement` |
| `0x00001000` | `direct_debit_advice` |
| `0x00002000` | `payment_status_report` |
| `0x10000000` | `protocol` |

Bit `0x00000001` is not defined. Aliases: 0 has two names ("none" and
"account default"); handled through the download access `0x00001DFE`; handled
through the order access `0x00002200`; everything `0x7FFFFFFF`.

### Bookkeeping-service flags

Bit set, 32 bits. One member: `0x1` `disabled`.

### Filter period

Plain enumeration, stored in one byte.

| Value | Name |
|---|---|
| 0 | `none` |
| 1 | `specified` (use `min_date` and `max_date`) |
| 2 | `last_month` |
| 3 | `last_quarter` |
| 4 | `last_half_year` |
| 5 | `last_year` |
| 6 | `last_two_years` |
| 7 | `current_month` |
| 8 | `current_quarter` |
| 9 | `current_year` |
| 10 | `previous_month` |
| 11 | `previous_quarter` |
| 12 | `previous_year` |

13 is the member count, not a value. The exact day arithmetic behind "last"
versus "previous" was not worked out.

### Filter amount mode

Bit set, stored in one byte.

| Value | Name |
|---|---|
| `0x0` | `ignore_amount` |
| `0x1` | `include_credit` |
| `0x2` | `include_debit` |

Alias: both `0x3`.

### Transaction kind

Plain enumeration, stored in one byte in the filter. It belongs to the booking
record, where it is the `transaction_code` field, an `i32` (04, sections 6.1
and 11.7; the same values). Listed here because the filter stores it.

| Value | Name |
|---|---|
| 0 | `other` |
| 1 | `remittance` |
| 2 | `debit` |
| 3 | `withdrawal_or_deposit` |
| 4 | `check` |
| 5 | `standing_order` |
| 6 | `reversal` |
| 7 | `charges` |
| 8 | `cash_management` |
| 9 | `card_payment` |

10 is the member count.

### Booking colour flag

Plain enumeration; the filter's `flag_mask` has one bit per value. It belongs
to the booking record, where it is the `flag` field, and to the payment's
`color_flag` (04, section 11.6; the same values).

| Value | Name |
|---|---|
| 0 | `none` |
| 1 | `ok` |
| 2 | `red` |
| 3 | `blue` |
| 4 | `yellow` |
| 5 | `green` |
| 6 | `gray` |
| 7 | `purple` |

8 is the member count; −1 is an in-memory "undefined".

### Retention unit

See [Retention period](#retention-period): 0 `days`, 1 `calendar_months`,
2 `calendar_years`.

## Verification

The record layouts of Part 2 are confirmed on test files produced with
Banking4. The test files hold synthetic data and were produced with the
Banking4 version described here; their expected results hold for a German
culture. `b4` itself decodes the account and the account group; it decodes
neither the booking filter nor the setting.

### Test data

1. The fixtures in `tests/fixtures/records/`, test files produced with
   Banking4: 63 records (6 base-only, 24 accounts, 7 groups, 15 filters, 11
   settings), per record the field values that were set before the record
   was written and the expected result of reading the written bytes. Every
   stored field of the five record types is set in them.
2. The demo vault in `tests/fixtures/vault/`, a test file produced with
   Banking4, for the two points of Part 1 named under
   [Not confirmed](#not-confirmed).
3. One real vault, see
   [Compared with a real vault](#compared-with-a-real-vault).

### Confirmed

Confirmed for the Banking4 version described here:

- **Record base.** Both fields, with zero, negative and extreme values; an
  update stamp of −1 is replaced by the current time when written, other
  values are not.
- **Account.** The complete field sequence of the table in both directions,
  with no byte unexplained in either chunk; the writer's constants for the
  unused fields and the version byte 26; the two-chunk structure; the early
  exit below version 2 and its consequences; all five processing steps,
  including the cash and crypto steps and the exact conditions of each; the
  precedence of tick-precision over day-precision time stamps; clamping of
  the day-precision dates; the address split and its trimming; the list
  with its presence flag; both wire forms of the retention period in both
  directions and the purge dates derived from them; truncation of the
  16-bit fields and of the codes on write.
- **Account group, booking filter, setting.** The complete field sequence
  in both directions; for the filter also the sanitising of the dates, the
  single-space convention for empty categories in both directions, the
  byte-wide enumerations, and the sentinel of the derived search amount.
- **Short and long records.** Everything in
  [Short, long and damaged records](#short-long-and-damaged-records), for
  all five record types: defaults for missing fields, unknown trailing bytes
  ignored, bytes behind the last chunk ignored, an unknown chunk ignored
  when the record is read and fatal to the load of the file after that
  record.
- **Loaders.** Records of kind none are dropped from the account file (the
  test is on the low eight bits); the group, filter and settings loaders
  keep every record; a repeated id gets a fresh one; a fresh id is the
  current stamp or the highest id read so far plus one, whichever is
  larger, and the id of a dropped record counts for that although it does
  not reserve that id; fewer than four stray bytes at the end are ignored;
  an unread chunk stops the load with that record kept; a truncated file
  and a bad record size stop it too.
- **Enumerations.** Every member of every stored enumeration and flag set,
  and values outside the defined members, which are kept unchanged within
  the stored width. The tables of this document, aliases included, hold
  every member that the Banking4 version described here has, with its
  value; none is missing or different.
- **Currency table.** The crypto entries of the table in the Banking4 version
  described here are `ETH`, `LTC`, `XBT` and `XRP`; a lookup by `BTC` or by a
  lower-case code finds nothing.

### Corrections made

No field, type, order or condition of the tables had to be changed. What
changed are things the document did not say, or said too loosely:

1. **Text form of the retention period.** The count is parsed by the
   integer parser of the runtime Banking4 runs on; the document now gives
   what that accepts, and that anything else makes Banking4 fail instead of
   yielding some default.
2. **Negative list count in the account.** Banking4 fails on it; the
   document had no rule. The filter's count was already described
   correctly (≤ 0 means no list). Counts are not checked against the bytes
   left; a rule for readers was added.
3. **Purge dates are computed by the reader,** and a text-form period
   reaching back before the year 1 makes the read fail.
4. **Below version 2 the reader does not advance** to the second chunk, so
   such a record with a second chunk stops the loader.
5. **Search amount of the filter.** The sentinel is now named and the
   parser declared out of scope; "parsing as an amount" was not something a
   reader could implement.
6. **Exact conditions of processing steps 4 and 5:** primary currency
   codes only and case-sensitive; ASCII digits only and exactly eight
   characters; "exactly cash" and "exactly bank" refer to the low eight
   bits of the stored kind.
7. **Trimming of the address parts** is now defined, as is the treatment of
   carriage returns.
8. **Retention period details:** which form keeps the unit of a "not set"
   or "unlimited" period (the document said "not set" is written as `D0`,
   which holds only for unit days: the text form writes `M0` and `Y0` and
   reads them back with their unit); a negative day count gives one month;
   the writer stores "unlimited" in the day form for a unit outside the
   enumeration; unit letters are case-sensitive.
9. **Filter details:** an empty category list is written like no list; only
   a string of exactly one space is turned into the empty string; the
   in-memory search amount and preset marker are never written.
10. **Time stamps:** a tick value of a single tick already supersedes the
    day; out-of-range tick values fail even in the unused field 63.
11. The earlier version of this section listed processing steps 2 and 4,
    the reading of the filter and the clamping of dates as not confirmed.
    They are confirmed now, and they behave as described.

### Not confirmed

- **Part 1.** The file map, the `Accts` migration, the start-up load order,
  the clean-up, the list of settings keys and the list of secrets are not
  confirmed. Only the loader behaviour listed above is confirmed; whether
  anything about it differs when the file is loaded as part of a vault is
  not confirmed. Two points are confirmed on a test file produced with
  Banking4, the demo vault (`tests/fixtures/vault/`): the kind-0
  placeholder in `Accts` is ignored when `AcctsNew` exists, and the
  settings file is read as a side effect of loading the exchange rates.
- **Record version 27.** What the ten additional bytes hold. That they are
  there, and zero, was seen again when the implementation read the real
  vault (below); the known fields of those records decode as described
  here.
- **Lookup of settings by name** (the later record wins, records without a
  name are unreachable) and removal of a setting by storing null.
- **Presets of the booking filter** not being written by their collection.
- **Editions.** The expected results of the fixtures in
  `tests/fixtures/records/` are those of Banking4's default product variant
  (no manifest parameters). That is **not** the full edition, as this
  document used to say: Banking4 loads
  portfolio transactions, payers, batches and invoices only when the manifest
  parameter for the product variant selects the professional or the business
  edition. None of the five record types of this document is among them, so
  the fixtures of this document are not affected. That the filter's category
  list is dropped in editions without categories is not confirmed.
- **The amount parser** behind the filter's search amount, beyond a
  handful of German-format examples.
- **Error handling around the load of a file.** That the runtime errors
  listed as fatal are caught and reported as a corrupt file rests on 02.
- **Other platforms.** Three things are decided by the runtime Banking4 runs
  on rather than by Banking4 itself: the integer parser of the text form
  (sign characters follow the culture), the string trimming of the address
  split, and the kind of error raised for a negative list count (an
  out-of-memory error, which may be another error on another platform).
  They may differ between the platforms Banking4 runs on and are not
  confirmed for all of them (open question 13).
- **Malformed UTF-8.** How many U+FFFD a malformed string yields. That is
  decided by the runtime Banking4 runs on (02, open question 8).
- **Very large list counts.** That Banking4 allocates by the count is
  confirmed. What it does with a very large count, where the outcome is a
  matter of memory, is not.
- **Older writers.** That a file written by an old program version reads
  by the short-read rule is not confirmed on a real one. Whether old
  versions wrote other values into the fields now called unused is unknown.
- **Real data for groups, filters and settings.** The real vault that was
  compared (below) has no `AcctGroups` file, and the Banking4 export used
  for comparison had no filters and no settings; `b4` reads neither of the
  two. These three record types are confirmed on test files produced with
  Banking4 only.

### Compared with a real vault

`b4`'s output for one real vault, written by a newer program version than
the Banking4 version described here, was compared with data exported from
Banking4 for the same vault, and no differences were found. All 21 account
records equal the export value by value. The records carry `record_version` 27
with 10 unread bytes at the end of chunk 1, all zero. The database written from
the vault equals the export in every compared value of the accounts and the
bookings (1.34 million values).

The comparison covers what the export holds: it has neither the bank access
fields nor `reserved_text`, and `b4` drops the access fields at decode time,
so those fields are confirmed on test files produced with Banking4 only. It
is one vault of one program version, and it says nothing about the loader
rules for dropped or damaged records.

## Open questions

1. **Record version 27.** What are the ten bytes that newer program versions
   append to chunk 1 of the account record? The Banking4 version described here
   is older than the program that last wrote the examined vault, so this
   document cannot say, and the Banking4 export used for comparison does not
   show them either. Needs a newer build of Banking4, or a vault where the
   bytes are non-zero. Other record types have grown in the same way: in the
   same vault payee and portfolio item records each end with 2 unread zero
   bytes (05, open question 12). The implementation saw the ten bytes again,
   zero in all 21 account records of that vault, and counts such bytes per
   record type in a warning.
2. **Why `AcctsNew`.** What happens is known, why is not. A plausible
   reading: old program versions cannot cope with current account records, and
   the placeholder keeps them from treating the vault as empty. The second
   chunk of the account record, which holds a single constant byte, would make
   an old reader that does not advance to it fail with the compatibility
   error; it may exist for exactly that purpose. Neither is confirmed.
3. **Time zone of `ticks` fields.** The tick values carry no zone. Whether
   the account's sync, download and balance times are local time, German time
   or UTC is not known.
4. **`reserved_text`** and the unused fields 15, 22–26, 63 and 69: earlier
   meaning unknown. Old vaults may hold non-default values there.
5. **Bank access fields of the account record.** Settled:
   `fints_customer_id`, `fints_account_sub_id`, `contact_name`,
   `order_contact_name`, the two contact GUIDs and `last_download_token` are
   stored in the account record, not in the bank access entries, but they
   are a login identifier, a continuation token and the identification of a
   stored bank access. `b4` drops all seven at decode time and never exports
   them (see [Entries](#entries)). The entry is kept so that the numbering
   of the list stays stable.
6. **Crypto currency codes** for processing step 4 come from a table inside
   the application, not from the vault, and may differ between versions. For
   the Banking4 version described here the four codes and the primary-code-only
   lookup are confirmed. A reader may skip step 4 for vaults written by current
   versions, since they store the crypto kind directly; which version first did
   so is unknown.
7. **`Acct<oid>/StmtMasterEntries`.** Settled as far as the format goes:
   Banking4 reads the records of the file exactly as it reads those of
   `Stmt` and adds them to the same collection, so `b4` reads it as more
   bookings and says so in a warning. This is not confirmed; no vault with
   such a file was available, and which program versions wrote it is
   unknown.
8. **Entries created by other components.** The map covers every entry used
   by the part of Banking4 that manages the vault's data. No other part of
   the application is known to create entries in the vault, but that is not
   confirmed.
   A reader should warn about any root entry not in the table.
9. **Name matching.** Settled by 01, section 7.3: entry names are compared
    exactly and case-sensitively. What remains open is only whether any
    program version ever wrote the fixed names of the file map in another
    spelling; none is known.
10. **Obsolete entries** `Layout`, `TanLists`, `ExchangeRates`, `Traces`:
    their formats are unknown; the Banking4 version described here only deletes
    them.
11. **Settings keys.** The list of secret keys covers the part of Banking4
    that manages the vault's data. Other parts of the application can store
    further keys through the same interface; they are not covered.
12. **Unparseable retention period texts.** Banking4 fails on
    them, which stops the load of the whole account file. Whether any
    program version ever wrote a text the current parser rejects (another
    unit letter is harmless, a missing count is not) is unknown. `b4` should
    keep the raw text and not fail.
13. **Other platforms.** The integer parser of the text form, string
    trimming and the error raised for a negative list count belong to the
    runtime Banking4 runs on, not to Banking4 itself. They may differ
    between the platforms Banking4 runs on and are not confirmed for all of
    them (see also 02, open question 8); the sign characters the parser
    accepts also follow the culture of the installation. None of this
    affects records Banking4 writes.
14. **Amount parser.** The rules by which a filter's search text becomes a
    search amount depend on an application-wide amount format and were not
    worked out. Not needed for reading a vault.
15. **Editions.** Which stored fields other editions leave unused, beyond
    the filter's category list, was not examined. Known since: the default
    product variant does not load portfolio transactions, payers, batches
    and invoices; the professional and the business edition do (see "Not
    confirmed"). Which other lists or fields depend on the variant is
    still open.
