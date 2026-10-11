# 05 Master data and portfolio

Record layouts for the master data lists (payees, payers, categories, mandates,
exchange rates, invoices), the securities data (stock instruments, price
series, portfolio items and transactions, tax exemptions, assets), the
documents attached to an account, the signature folder, and two logs.

Everything here describes the behaviour of one Banking4 9.x build, for the
purpose of reading a user's own vault. The text calls it "the Banking4 version
described here"; it is the build named in 02 section 0. Field, record and
enumeration names are ours. Section 8 says which statements are confirmed and
on what; the ones that are not are marked *(not confirmed)* where they matter.
Section 9 lists what is still open. All byte examples are synthetic.

Related documents: 02 (record framing and primitives), 03 (accounts), 04 (the
payment base that the signature folder item builds on).

## 1. Where the records live

All of these are binary collection files as described in 02 section 1: a
sequence of `[i32 record_size][record]`, no header, no trailer.

| Vault path | Record | Scope | Opened by the application |
|---|---|---|---|
| `Payees` | payee (3.1) | global | unconditionally |
| `Payers` | payer (3.2) | global | unconditionally |
| `Categories` | category (3.3) | global | unconditionally |
| `Mandates` | mandate (3.4) | global | unconditionally |
| `ExchangeRatesBIN` | exchange rate (3.5) | global | unconditionally |
| `StockInstruments` | stock instrument (3.6) | global | unconditionally |
| `Stock<oid>/StockPrices` | stock price (3.7) | one file per stock instrument | when that instrument's prices are first needed |
| `Acct<oid>/Portf` | portfolio item (3.8) | one file per account | if `account_kind` is exactly `portfolio` |
| `Acct<oid>/PortfTxns` | portfolio transaction (3.9) | one file per account | if `account_kind` is exactly `portfolio` |
| `Acct<oid>/AcctTaxExemptions` | tax exemption (3.10) | one file per account | for any account kind |
| `Acct<oid>/Assets` | asset (3.11) | one file per account | if `account_kind` is exactly `asset` |
| `Acct<oid>/AcctFiles` | account document (3.12) | one file per account | if any of the seven kind bits is set |
| `Acct<oid>/<document oid><ext>` | content of one account document (3.12.1) | one file per document | (not a record file) |
| `Acct<oid>/AcctPayChecks` | signature folder item (3.13) | one file per account | if the `bank` bit of `account_kind` is set |
| `Acct<oid>/AcctPayCheckSplits` | signature folder split (3.14) | one file per account | if the `bank` bit of `account_kind` is set |
| `Invoices` | invoice (3.15) | global | unconditionally |
| `OnlineTraces` | online trace (3.16) | global | unconditionally |
| `ImportExportPaymtsLog` | import/export log entry (3.17) | global | unconditionally |

`<oid>` is the decimal object id of the owning record: of the account for
`Acct<oid>`, of the stock instrument for `Stock<oid>`. In the container
`Acct<oid>` and `Stock<oid>` are directory entries of the root directory and
the file is an entry of that directory (document 01, section 7). No
separator character is stored anywhere; the `/` is only this document's
notation (see document 03, "Paths").

The last column describes when the application opens the file; document 03
has the account kinds and the order in which files are loaded. A missing file
is simply an empty list. A reader should go by what exists in the vault: list
`Acct<oid>/` and `Stock<oid>/` and decode what it finds, rather than replicate
the conditions. The application can also be built with whole features switched
off (payees, categories, documents, signature folder, portfolio transactions,
tax exemptions); such a build never opens the corresponding files. That is a
property of the build, not of the vault. The ordinary product has such a
switch as well: under its default product variant Banking4 does not load
`Payers`, `Invoices` and `Acct<oid>/PortfTxns` (nor batches, 04); it does so
only when the manifest parameter for the product variant selects the
professional or the business edition (03, "What is loaded when").

`b4` does not read `OnlineTraces`: it is one of the nine entries that are
never read (03, "Entries"). Its layout (3.16) is given for completeness.

Two retired top-level names are deleted by the application's vault clean-up and
are not record files of this document: `ExchangeRates` (the predecessor of
`ExchangeRatesBIN`) and `Traces`. *(not confirmed)*

## 2. Common structure

### 2.1 Primitives

The primitive names are those of 02 section 4. The ones used here:

| Primitive | Bytes | Short reminder |
|---|---|---|
| `u8` | 1 | unsigned |
| `bool` | 1 | 0 is false, anything else true |
| `u16` | 2 | unsigned; the writer keeps the low 16 bits of a wider value |
| `i32` | 4 | signed |
| `decimal` | 16 | 96 bit mantissa, scale, sign |
| `string` | 2+n or 6+n | UTF-8; length 0 is "no value" |
| `code(n)` | n | fixed width, one byte per character, NUL padded; trimmed on read; empty is "no value" |
| `blob` | 2+n or 6+n | bytes; length 0 is "no value" |
| `date_raw` | 4 | year (`u16`), month, day; all zero is "no date"; not validated |
| `ticks` | 8 | 100 ns units since 0001-01-01, no time zone; 0 is "no timestamp" |
| `time_hms` | 3 | hour, minute, second; hour `FF` is "no time" |

Every calendar date in this document is a `date_raw`; none of these records
uses the clamping or the strict date reader of 02. Documents 03 and 04 use
the same primitive names.

A read that does not fit into the rest of the current chunk returns the default
(0, false, "no value", "no date", "no time") and does not move, see 02
section 3. All layouts below rely on that: a record written by an older version
is simply shorter, and the missing tail reads as defaults.

None of the seventeen records of this document has a counted list (a count
followed by that many elements); the only lengths are those of `string` and
`blob`, which 02 bounds by the rest of the chunk. Should a later version add
one, the rule of 02 section 8, rule 11 applies, as it does in 03 and 04:
never allocate by the count; refuse a count larger than the bytes left in the
chunk divided by the smallest size of one element; compare by division or in
checked arithmetic.

### 2.2 Object header

Every record starts with the two values of the common base of all record types.
In the tables they are positions 1 and 2. They are read before anything else:
the base part is always read first. In the single-chunk records the fields of
the concrete record type follow directly in the same chunk; in the signature
folder item the payment base follows and the item's own fields are in chunk 2.

Several of these records (mandate, portfolio item, portfolio transaction,
asset, signature folder split, invoice, and the payment base) build on the
intermediate monetary base that document 03 describes. That base reads and
writes nothing, so it does not show up in the tables.

`oid` and `update_stamp` are stamps as described in 02 section 5.1.

### 2.3 Chunks

Sixteen of the seventeen record types are **single-chunk** records: their
reader never asks for a next chunk, and their writer never starts one. The
"Chunk" column of their tables is therefore always 1. If such a record carried
the "more chunks follow" flag, the application would refuse the whole file
after decoding it (02 section 7); `b4` skips surplus chunks instead (02
section 8).

The one exception is the signature folder item (3.13), which has two chunks
like the other members of the payment family.

### 2.4 How to read the tables

- "The reader" and "the writer" stand for Banking4 reading and writing one
  record; "the loader" stands for Banking4 loading a whole collection file.
- **Position** counts the wire reads of the record in the order the reader
  performs them, starting with the object header.
- Rows without a position are steps that read nothing (a test, a chunk change).
- **Notes** mention everything the reader does to the value. Longer rules are
  in the sub-sections after each table.
- "After reading" lists changes the record reader makes once all fields are
  read. "Loader" lists what Banking4 does with a decoded record before it is
  added to the list; a record the loader drops does not appear in the
  application at all.
- "Writer" describes the writing side: unless a difference is listed, the
  writer emits the same primitives in the same order, each from the field the
  reader filled.

## 3. Record layouts

### 3.1 Payee

File `Payees`, global. An entry of the address book of payment counterparties,
also usable as a payment template (default purpose, amount, category).

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `name` | Name of the payee. |  |
| 4 | 1 | `string` | `account_number` | National account number. |  |
| 5 | 1 | `string` | `bank_code` | National bank code. |  |
| 6 | 1 | `string` | `purpose` | Default remittance text. |  |
| 7 | 1 | `string` | `category` | Category full name (section 4). | Normalised on read (4.2). |
| 8 | 1 | `string` | `iban` | IBAN. | Banking4 also indexes payees by this value. |
| 9 | 1 | `string` | `bic` | BIC. |  |
| 10 | 1 | `string` | `notes` | Free user notes. |  |
| 11 | 1 | `string` | `address` | Postal address: street line, LF, town line. | Split and joined as described in 3.1.1. |
| 12 | 1 | `code(2)` | `country` | ISO 3166 country code. |  |
| 13 | 1 | `string` | `bank_name` | Name of the payee's bank. |  |
| 14 | 1 | `string` | `bank_address` | Bank address: street line, LF, town line. | Same split and join as `address`. |
| 15 | 1 | `string` | `mandate_id` | Legacy SEPA mandate reference. | A payee that still carries one is turned into a payer after load (3.1.2). |
| 16 | 1 | `date_raw` | `mandate_signature_date` | Legacy mandate signature date. |  |
| 17 | 1 | `string` | `reserved_text` | Unused slot. | Read and discarded. The writer always writes "no value". |
| 18 | 1 | `code(3)` | `currency` | Currency of the default amount. |  |
| 19 | 1 | `decimal` | `amount` | Default amount. |  |
| 20 | 1 | `string` | `email` | E-mail address. |  |
| 21 | 1 | `string` | `customer_number` | Customer number at the payee. |  |
| 22 | 1 | `u16` | `payee_kind` | Enum `payee_kind`. | Reader keeps only 0, 1, 2 and 4; any other value becomes 0. |
| 23 | 1 | `string` | `local_instrument_code` | Legacy mandate: SEPA local instrument code. |  |
| 24 | 1 | `date_raw` | `mandate_revocation_date` | Legacy mandate revocation date. |  |
| 25 | 1 | `u16` | `payee_flags` | Flags `payee_flags`. |  |
| 26 | 1 | `u8` | `vop_status` | Enum `vop_status`: last verification-of-payee result. |  |
| 27 | 1 | `string` | `vop_matched_name` | Name reported back by verification of payee. |  |

**After reading.** `payee_kind` is kept only if it is 0, 1, 2 or 4. Any other
value, including 8 and combinations, is replaced by 0.

**Loader.** Every record is kept. Banking4 additionally indexes payees by
`iban` (several payees may share one). After loading, the list is sorted by
`name` without regard to case, compared in the language (culture) of the
installation, then by a supplemental name, then by `iban`, then by `oid`.
Which value the supplemental name is was not established: the table above
has no field of that name. (The order by name is confirmed on a test file
produced with Banking4: the payee file of the demo vault in
`tests/fixtures/vault/` is sorted by name, and Banking4 keeps that order
when it loads the file. The order by name depends on the installation, so a
reader cannot reproduce it in general and should sort for itself.)

**Not stored.** A digest of the name, the initials shown in the avatar, and a
link to an own account are computed in memory.

**Writer.** Same order and primitives, with these differences:

- Position 17 is written as "no value" whatever was read.
- `address` and `bank_address` are joined from two fields (3.1.1).
- `payee_kind` and `category` are written as held in memory, so the
  replacement above and the category normalisation become permanent on the
  next save.

#### 3.1.1 Address fields

The application keeps a street line and a town line; the file keeps one
`string`.

Reading:

1. "No value" gives no street and no town.
2. If the text contains no LF, the whole text is the street line, untrimmed,
   and there is no town line.
3. Otherwise the text is cut at the **first** LF. The part before it, trimmed
   of white space, is the street line; the part after it, trimmed, is the town
   line. A part that is empty after trimming is "no value". Further LFs stay
   inside the town line.

Writing: if both lines are "no value" the field is "no value"; otherwise it is
street line, one LF, town line, with a missing line written as empty text. So
a payee with only a town is written as LF followed by the town, and one with
only a street as the street followed by LF. The lines are written as held,
untrimmed. Two consequences:

- Lines that are held as empty texts rather than as "no value" produce a lone
  LF, which reads back as no street and no town.
- A line break inside the street line moves on the next read: the text is cut
  at its first LF, so everything after it becomes part of the town line.

#### 3.1.2 Legacy mandate fields

`mandate_id`, `mandate_signature_date`, `local_instrument_code` and
`mandate_revocation_date` date from a time when a direct debit mandate was
stored with the payee. Right after `Payees` is loaded, and if the payer list is
available, the application walks all payees and, for each one whose
`mandate_id` has a value, creates a payer record (3.2) from it and clears the
mandate data of the payee. *(not confirmed)*

A reader can treat a payee with a `mandate_id` as "payee plus an implied
payer". In a vault that a current version has saved, these fields are normally
empty.

### 3.2 Payer

File `Payers`, global. A debtor from whom the user collects by direct debit:
a reference to a payee record plus the mandate data.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `i32` | `party_oid` | `oid` of the payee record that holds name, address and bank details of this payer. | Looked up in `Payees` on first use. |
| 4 | 1 | `string` | `party_name` | Copy of the party's name. |  |
| 5 | 1 | `string` | `mandate_id` | SEPA mandate reference. |  |
| 6 | 1 | `date_raw` | `mandate_signature_date` | Date the mandate was signed. |  |
| 7 | 1 | `date_raw` | `mandate_revocation_date` | Date the mandate was revoked. |  |
| 8 | 1 | `string` | `local_instrument_code` | SEPA local instrument code. |  |
| 9 | 1 | `string` | `notes` | Free user notes. |  |
| 10 | 1 | `string` | `category` | Category full name (section 4). | Normalised on read (4.2). |

**After reading.** Nothing beyond the category normalisation.

**Loader.** Every record is kept.

**Not stored.** Account number, bank code, address and so on are taken from the
referenced payee when needed.

**Writer.** No difference.

### 3.3 Category

File `Categories`, global. Section 4 explains how categories work; this section
is the layout only.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `full_name` | Full hierarchical name, segments separated by ":" (section 4). | Normalised on read (4.2); for special categories 1 and 2 only the last level is kept (3.3.1). |
| 4 | 1 | `i32` | `layout_version` | Version of the category record layout. | Writer always writes 9. Only steers reading of fields 6 and 9; not kept. |
| 5 | 1 | `string` | `description` | Free description. |  |
| 6 | 1 | `string` | `rules` | Keyword expression for automatic categorisation (section 4.5). | Parsed according to `layout_version`. "No value" means no rules. |
| 7 | 1 | `string` | `reserved_text` | Unused slot. | Read and discarded. The writer always writes "no value". |
| 8 | 1 | `u8` | `totals_bucket` | Enum `totals_bucket`. | Forced to 1 for special categories 1 and 2 (3.3.1). |
| 9 | 1 | `u8` | `budget_period` | Enum `budget_period` (section 4.4). | If `layout_version` < 6 the stored byte is ignored and 2 (month) is used. Forced to 0 for special categories 1 and 2. |
| 10 | 1 | `decimal` | `budget_amount` | Budget amount for one `budget_period` (section 4.4). | Forced to 0 for special categories 1 and 2. |
| 11 | 1 | `decimal` | `vat_percent` | VAT rate in percent that bookings of this category get by default. |  |
| 12 | 1 | `string` | `icon_name` | Name of the category icon. | Forced to a fixed name for special categories 1 and 2 (3.3.1). |
| 13 | 1 | `string` | `color_name` | Name of the category colour. |  |
| 14 | 1 | `u8` | `special_category` | Enum `special_category` (section 4.3). |  |
| 15 | 1 | `u8` | `reserved_byte` | Unused slot. | Read and discarded. The writer always writes 0. |

**After reading.** See 3.3.1.

**Loader.** *(not confirmed)*

- A record whose `full_name` is "no value" after reading is dropped.
- For each of the three special values of `special_category`, the first record
  that carries it becomes *the* special category of that kind. Any later record
  with the same value is kept, but its `special_category` is set to 0.
- After the file is loaded, and only if it yielded at least one category, the
  application adds the special categories that are still missing (4.3).

One case of this is confirmed on a test file produced with Banking4: the demo
vault (`tests/fixtures/vault/`) holds all three special categories, and
Banking4 adds none and changes none when all three exist. So existing special
categories are not duplicated. Dropping, the handling of a repeated special
value and the adding of missing ones are still not confirmed.

**Not stored.** `pseudo_category` (section 6) is 0 for every record read from a
file.

**Writer.** Same order and primitives, with these differences:

- `layout_version` is always written as 9.
- `rules` is not the text that was read but a canonical text regenerated from
  the parsed rules (4.5); when there are no rules it is "no value".
- Positions 7 and 15 are written as "no value" and 0.
- The adjustments of 3.3.1 and the version rule for `budget_period` are applied
  by the reader only; the writer emits whatever is in memory.

#### 3.3.1 Adjustments after reading

Depending on `special_category`:

| Value | `full_name` | `totals_bucket` | `budget_period` | `budget_amount` | `icon_name` |
|---|---|---|---|---|---|
| 1 | reduced to its last level (the text after the last `:`) | 1 | 0 | 0 | `reclass` |
| 2 | reduced to its last level | 1 | 0 | 0 | `exclude` |
| 0, 3, other | unchanged | unchanged | unchanged | unchanged | unchanged |

So the special categories 1 and 2 are always top-level categories that are
excluded from totals and have no budget, whatever the file says. The forced
`budget_amount` is a plain zero with scale 0 (`0`), not `0.00`.

Independently of that, `budget_period` is taken from the file only when
`layout_version` is 6 or higher. For a lower version the byte is still consumed
but the value 2 (month) is used.

### 3.4 Mandate

File `Mandates`, global. A direct debit mandate under which a creditor debits
one of the user's accounts. The application consults the list when it suggests
a category for a booking that carries a mandate reference; where the entries
come from was not established.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `creditor_id` | SEPA creditor identifier of the party that debits the own account. |  |
| 4 | 1 | `string` | `mandate_id` | SEPA mandate reference. |  |
| 5 | 1 | `string` | `category` | Category full name (section 4). | Normalised on read (4.2). |
| 6 | 1 | `string` | `creditor_name` | Name of the creditor. |  |
| 7 | 1 | `i32` | `mandate_flags` | Flags `mandate_flags`. |  |
| 8 | 1 | `ticks` | `mandate_date` | Date associated with the mandate. | Which date this is was not established (open question 2). |

**After reading.** Nothing beyond the category normalisation.

**Loader.** *(not confirmed)* A record is dropped when

- `mandate_id` or `creditor_id` is "no value", or
- `mandate_id` is exactly `OFFLINE`, or
- `creditor_id` equals one particular creditor identifier that is hard-coded
  in the application (the published SEPA creditor identifier of one large
  payment service). The comparison is exact.

`b4` does not replicate these three rules. They decide what the application
shows, not what the file holds, and a dropped record stays in the file until
the application saves the list again. A reader keeps every mandate record;
if it wants to offer the application's view it can mark the records the
rules would drop.

After loading, the list is sorted by `creditor_name` without regard to case.
(Not confirmed: the mandate file of the demo vault has a single record, which
shows nothing about the order.)

**Not stored.** The list of bookings that belong to the mandate is rebuilt in
memory.

**Writer.** No difference.

Before it reads anything of a mandate record, Banking4 prepares the in-memory
booking list, and that takes the application's default currency. This has no
bearing on the layout.

### 3.5 Exchange rate

File `ExchangeRatesBIN`, global. One quote per currency against the euro.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `code(3)` | `base_currency` | Base currency of the quote. | Loader keeps only EUR-based rates (see "Loader" below). |
| 4 | 1 | `code(3)` | `target_currency` | Quoted currency. |  |
| 5 | 1 | `ticks` | `quote_time` | Time of the quote. |  |
| 6 | 1 | `decimal` | `quote_rate` | Units of `target_currency` per one unit of `base_currency`. | See "Loader" below for the inversions applied by the loader. |

**After reading.** Nothing.

**Loader.** *(not confirmed)* Each decoded record goes through these steps
in order:

1. If `base_currency` is not `EUR` and `target_currency` is `EUR`, the pair is
   turned round: `target_currency` takes the old base, `base_currency` becomes
   `EUR`, and `quote_rate` becomes `1 / quote_rate`.
2. If the pair is now `EUR` to `XAU` and `quote_rate` is greater than 1,
   `quote_rate` becomes `1 / quote_rate`.
3. If `base_currency` is still not `EUR`, the record is dropped.
4. If `target_currency` is "no value" or is not exactly three upper-case
   letters `A` to `Z`, the record is dropped. (A lower-case code is dropped
   too. No list of currencies is consulted.)
5. If a rate for the same `target_currency` was already loaded, the record is
   dropped (the first one wins).

After loading, the list is sorted by `target_currency`, compared ordinally
(by code unit). This is confirmed on a test file produced with Banking4: the
demo vault's file is in that order, and Banking4 keeps it on loading.

`b4` applies none of the five steps to what it stores: every record is kept
with the currencies and the rate the file holds. Steps 1 and 2 divide by a
stored value that can be zero, and nothing says with what scale or rounding
the quotient is formed. A helper that offers the inverted rate must use
checked decimal arithmetic and return "no value" for a zero rate or a result
that does not fit; it must never abort and never drop the record.

Loading this list has a side effect in the application: Banking4 reads its
settings file (`ModelSettings`) while it loads the exchange rates (03, "What
is loaded when"). `b4` does not.

**Not stored.** A "fixed rate" flag and a currency class (active, historic,
precious metal, crypto) exist in memory only; the record reader leaves both at
their defaults.

**Writer.** No difference.

### 3.6 Stock instrument

File `StockInstruments`, global. One record per security for which the vault
keeps a price series.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `isin` | ISIN of the security, 12 characters. | Parsed on read (5.1). The writer emits the 12 character canonical ISIN of 5.1 or "no value". |
| 4 | 1 | `string` | `name` | Name of the security. |  |
| 5 | 1 | `code(3)` | `price_currency` | Currency of the price series. |  |
| 6 | 1 | `string` | `splits` | Stock splits, one per LF-separated line (3.6.1). | "No value" when there are none. |

**After reading.** `isin` is parsed as described in 5.1. `splits` is parsed
into a list (3.6.1).

**Loader.** Every record is kept. A record whose parsed identifier yields an
ISIN is entered into an index keyed by that ISIN. A second record with the same
ISIN makes the application's loader fail at that point, so the rest of the file
is not loaded *(not confirmed)*. A reader should tolerate duplicates and
report them.

**Writer.** Same order and primitives, with these differences:

- `isin` is written in canonical form (5.1), which is "no value" for
  identifiers that are neither an ISIN nor a German national number of six
  characters. So the identifier forms of 5.1 other than a plain ISIN can be
  met in this field only in a file that something other than a current writer
  produced; they are accepted on reading all the same.
- For an identifier whose canonical form cannot be derived (5.1, last
  paragraph) the writer fails altogether. The canonical form is not always a
  valid ISIN either: `DE000abcdef2` is written for `WKN abcdef` and is "no
  identifier" when it is read again.
- `splits` is regenerated from the parsed list (3.6.1).

#### 3.6.1 Splits

`splits` is "no value" or a text of one or more lines separated by LF. Each
line has three tokens, each terminated by `/`:

```
<yyyymmdd>/<R or empty>/<factor>/
```

- token 1: the split date, eight digits;
- token 2: `R` for a reverse split, empty for an ordinary split;
- token 3: the split factor, a decimal integer.

Example of two lines: `20200131/R/4/` and `20210228//10/`.

The writer produces exactly this form: the date without its time of day, `R`
or nothing, the factor in decimal, lines joined by one LF and no LF at the
end. A list without entries is written as "no value".

The reader cuts the text at every LF and parses every piece, including empty
ones. "No value" (which an empty text is, 02 section 4.8) yields no list. For
each piece it takes three tokens in turn; a token is the text up to the next
`/`, and it is missing when there is no further `/`.

| Token | Accepted | Otherwise |
|---|---|---|
| 1, date | exactly eight digits that form a real calendar date (year 0001 to 9999) | the reader fails |
| 2, direction | exactly `R` means reverse | anything else, including `r`, `RR` and a missing token, means an ordinary split |
| 3, factor | a decimal integer in the signed 32 bit range, optionally with a sign and with white space around it; zero and negative values are taken as they are | the reader fails (missing token, no number, out of range) |

Whatever follows the third `/` on a line is ignored (so a CR before the LF
does no harm). A failure here is a failure of the whole record: Banking4
stops reading the file at that record, and the records after it are not read
(02 section 7). In particular a text that ends with LF fails, because of the
empty piece after it. A reader for `b4` should report such a line and carry
on with the other fields.

Use of the list *(not confirmed)*: a price observed on a day **before** a
split date is adjusted to today's share count by dividing it by the factor for
an ordinary split and multiplying it by the factor for a reverse split, for
every split in the list. When a record stores no splits, the application falls
back to a table of known splits per ISIN that is embedded in the program and is
not in the vault.

The reader accepts a factor of zero, and this adjustment divides by the
factor. `b4` stores the splits and the prices as the file holds them and
applies no split adjustment to a stored value. A helper that computes an
adjusted price must use checked decimal arithmetic and return "no value" for
a factor of zero or a result that does not fit.

### 3.7 Stock price

File `Stock<oid>/StockPrices`, one file per stock instrument, where `<oid>` is
the `oid` of the stock instrument record. One record per quote.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `date_raw` | `date` | Trading day of the quote. |  |
| 4 | 1 | `decimal` | `price` | Price in the instrument's `price_currency`. |  |

**After reading, loader.** Nothing; every record is kept.

**Writer.** No difference.

Worked example, a complete record as it follows its `record_size` word
(`20 00 00 00`):

```
20 00 00 00   chunk header: size 32, no further chunk
FA 46 73 2D   oid 762529530
FB 46 73 2D   update_stamp 762529531
E8 07 02 1D   date 2024-02-29
D2 04 00 00 00 00 00 00 00 00 00 00 00 00 02 00   price 12.34
```

### 3.8 Portfolio item

File `Acct<oid>/Portf`, one file per portfolio account. One record per
position (holding) of the account.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `i32` | `item_state` | Flags `portfolio_item_state`. |  |
| 4 | 1 | `string` | `isin` | Security identifier as text, normally an ISIN (5.1). | Kept as stored. |
| 5 | 1 | `string` | `name` | Name of the security. | Reader replaces every LF by a space. |
| 6 | 1 | `date_raw` | `maturity_date` | Maturity date (bonds). |  |
| 7 | 1 | `decimal` | `interest_rate` | Interest rate. | Reader replaces negative values by 0. |
| 8 | 1 | `string` | `portfolio_key` | Key text of the position. | Meaning not established (open question 3). |
| 9 | 1 | `decimal` | `quantity` | Holding size, units or face amount depending on `quantity_type`. |  |
| 10 | 1 | `i32` | `quantity_type` | Enum `quantity_type`. |  |
| 11 | 1 | `code(3)` | `quantity_currency` | Currency of a face-amount quantity. | Replaced by "no value" on read when `quantity_type` is 2 (units). |
| 12 | 1 | `i32` | `price_type` | Enum `price_type`. |  |
| 13 | 1 | `code(3)` | `current_price_currency` | Currency of `current_price`. |  |
| 14 | 1 | `decimal` | `purchase_price` | Purchase price per unit (or percent). | Reader replaces negative values by 0. |
| 15 | 1 | `date_raw` | `purchase_price_date` | Date of the purchase price. |  |
| 16 | 1 | `decimal` | `current_price` | Latest known price per unit (or percent). | Reader replaces negative values by 0. |
| 17 | 1 | `date_raw` | `current_price_date` | Date of the latest price. |  |
| 18 | 1 | `time_hms` | `current_price_time` | Time of the latest price. |  |
| 19 | 1 | `code(3)` | `purchase_price_currency` | Currency of `purchase_price`. | A missing value may be filled on read (3.8.1). |
| 20 | 1 | `decimal` | `alert_lower_limit` | Price below which the user wants to be notified. | Reader replaces negative values by 0. |
| 21 | 1 | `decimal` | `alert_upper_limit` | Price above which the user wants to be notified. | Reader replaces negative values by 0. |
| 22 | 1 | `date_raw` | `blocked_until_date` | Date until which the position is blocked. |  |
| 23 | 1 | `string` | `additional_info` | Additional text lines, separated by LF. |  |

**After reading.** See 3.8.1.

**Loader.** Every record is kept. After loading, the list is sorted by `name`,
compared ordinally (by code unit), then by `oid`. This is confirmed on a test
file produced with Banking4: the demo vault's file is in that order, and
Banking4 keeps it on loading.

**Writer.** Same order and primitives. The adjustments of 3.8.1 are applied by
the reader only.

#### 3.8.1 Adjustments after reading

1. `name`: every LF is replaced by a space.
2. `interest_rate`, `purchase_price`, `current_price`, `alert_lower_limit`,
   `alert_upper_limit`: a negative value is replaced by 0. `quantity` is not
   guarded. The replacement is a plain zero with scale 0 (`0`), whatever the
   scale of the stored value was (not `0.00`).
3. `additional_info` is split into lines as in 02 section 5.2.
4. If `quantity_type` is 2 (units), `quantity_currency` becomes "no value".
5. If `price_type` is 1 (amount per unit) and `purchase_price` is greater than
   0 and `purchase_price_currency` is "no value", then
   `purchase_price_currency` is set to `current_price_currency`, or, if that is
   "no value" too, to the application's default currency (normally `EUR`).

### 3.9 Portfolio transaction

File `Acct<oid>/PortfTxns`, one file per portfolio account. One record per
securities transaction reported for the account.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `transaction_id` | Identifier of the transaction. |  |
| 4 | 1 | `i32` | `transaction_state` | Enum `portfolio_transaction_state`. |  |
| 5 | 1 | `i32` | `delivery_direction` | Enum `delivery_direction`. |  |
| 6 | 1 | `i32` | `activity` | Enum `transaction_activity`. |  |
| 7 | 1 | `string` | `isin` | Security identifier as text (5.1). |  |
| 8 | 1 | `string` | `name` | Name of the security. |  |
| 9 | 1 | `decimal` | `quantity` | Quantity moved. |  |
| 10 | 1 | `u8` | `quantity_type` | Enum `quantity_type`. | One byte here, four bytes in the portfolio item. |
| 11 | 1 | `ticks` | `settlement_date` | Settlement date. |  |
| 12 | 1 | `decimal` | `settlement_amount` | Settlement amount. |  |
| 13 | 1 | `code(3)` | `settlement_currency` | Currency of `settlement_amount`. |  |
| 14 | 1 | `bool` | `interest_accrued` | Whether accrued interest is part of the transaction. |  |
| 15 | 1 | `u16` | `interest_days` | Number of interest days. | Unsigned 16 bit on the wire. |
| 16 | 1 | `decimal` | `interest_amount` | Accrued interest amount. |  |
| 17 | 1 | `code(3)` | `interest_currency` | Currency of `interest_amount`. |  |
| 18 | 1 | `bool` | `is_read` | Whether the user has seen the transaction. |  |
| 19 | 1 | `code(3)` | `quantity_currency` | Currency of a face-amount quantity. |  |
| 20 | 1 | `ticks` | `trading_date` | Trade date. |  |
| 21 | 1 | `decimal` | `trading_price` | Trade price. |  |
| 22 | 1 | `code(3)` | `trading_currency` | Currency of `trading_price`. |  |

**After reading, loader.** Nothing; every record is kept.

**Writer.** No difference in order. `quantity_type` and `interest_days` are
narrowed to one and two bytes on writing.

Unlike the portfolio item, this record keeps its dates as `ticks`.

### 3.10 Tax exemption

File `Acct<oid>/AcctTaxExemptions`, one file per account. One record per tax
exemption order of the account, apparently the allowance for capital income
that a bank may pay out untaxed, with the part already used.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `ticks` | `valid_from` | First day the exemption order is valid. |  |
| 4 | 1 | `ticks` | `valid_until` | Last day the exemption order is valid. |  |
| 5 | 1 | `decimal` | `amount` | Exempted amount. |  |
| 6 | 1 | `decimal` | `used_amount` | Part of the amount already used. |  |
| 7 | 1 | `decimal` | `remaining_amount` | Part of the amount still available. | Stored, not derived from the two fields before. |

**After reading, loader.** Nothing; every record is kept.

**Writer.** No difference.

### 3.11 Asset

File `Acct<oid>/Assets`, one file per asset account. One record per asset
(for example a term deposit) of the account.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `asset_id` | Identifier of the asset. |  |
| 4 | 1 | `decimal` | `signed_amount` | Amount; a negative value marks a debit (liability). | Reader splits it into an absolute amount and a debit flag (3.11.1). |
| 5 | 1 | `code(3)` | `currency` | ISO 4217 currency code. |  |
| 6 | 1 | `date_raw` | `deposit_date` | Start date of the deposit. |  |
| 7 | 1 | `date_raw` | `expiry_date` | End date of the deposit. |  |
| 8 | 1 | `decimal` | `interest_rate` | Interest rate. |  |
| 9 | 1 | `code(1)` | `interest_method` | One character code for the interest method. | Code values not established (open question 4). |
| 10 | 1 | `string` | `description` | Description of the asset. |  |

**After reading.** See 3.11.1.

**Loader.** Every record is kept.

**Writer.** Same order and primitives; `signed_amount` is rebuilt from the two
in-memory values (3.11.1).

#### 3.11.1 Amount and direction

The file stores one signed `decimal`. In memory the application keeps an
unsigned `amount` and a `debit` flag:

- stored value below 0: `amount` is its negation and `debit` is set;
- otherwise: `amount` is the stored value and `debit` is clear.

The writer stores `-amount` when `debit` is set and `amount` otherwise. It
does not look at the sign of `amount`, so the pair is not always preserved:

- A zero amount with `debit` set is stored as a negative zero (sign bit set,
  magnitude 0; 02 section 4.7). That is not below 0, so it reads back as an
  amount of negative zero with `debit` clear: the flag is lost.
- A negative in-memory amount comes back positive with the flag inverted
  (`-10.5` without `debit` reads back as `10.5` with `debit`).

### 3.12 Account document

File `Acct<oid>/AcctFiles`, one file per account. One record per document
belonging to the account: statements in various formats, bank mail, files the
user attached. The record is the catalogue entry; the document itself is a
separate file (3.12.1).

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `i32` | `document_type` | Enum `document_type`. | Also selects the extension of the content file (3.12.1). |
| 4 | 1 | `ticks` | `document_date` | Date of the document. |  |
| 5 | 1 | `string` | `title` | User-visible title. | Replaced by "no value" on read when it equals the generated default title (3.12.2). |
| 6 | 1 | `string` | `additional_info` | Additional text. |  |
| 7 | 1 | `u8` | `document_bits` | Flags `document_bits`. |  |
| 8 | 1 | `string` | `file_name` | Original or suggested file name for export. | Not the name of the content file inside the vault. A text only: a reader never uses it as a path on the host file system. |
| 9 | 1 | `u8` | `document_status` | Enum `document_status`. |  |
| 10 | 1 | `i32` | `statement_number` | Statement number, 0 if none. |  |
| 11 | 1 | `i32` | `statement_year` | Statement year, 0 if none. |  |
| 12 | 1 | `string` | `document_id` | Identifier of the document at the bank. | Banking4 indexes documents by this value. |
| 13 | 1 | `u8` | `acknowledge_status` | Enum `acknowledge_status`. |  |
| 14 | 1 | `string` | `acknowledge_token` | Token needed to acknowledge receipt at the bank. | Bank protocol token: read to stay in step and dropped at decode time, see below. |
| 15 | 1 | `string` | `file_format` | Format name of the content. |  |
| 16 | 1 | `string` | `file_format_version` | Format version of the content. |  |
| 17 | 1 | `i32` | `outbox_oid` | `oid` of a related outbox order (04 section 10), 0 if none. |  |
| 18 | 1 | `blob` | `content_hash` | RIPEMD-160 digest of the content (20 bytes). | "No value" if never computed. |

**After reading.** See 3.12.2.

**Confidentiality.** The account document is not harmless catalogue data: it
contains a bank protocol token. `acknowledge_token` is what the application
presents to the bank to acknowledge a document; what else it authorises was
not established. It is classified with the bank access fields of the account
and the payment (03, "Entries"). `b4` does not decode account documents yet.
On the day it does, the decoder reads position 14 only to advance and
discards the value: it is never kept in a memory structure, never written to
the database and never exported.

**Loader.** Every record is kept. Records whose `document_id` has a value are
indexed by it; if two records share an id, the later one replaces the earlier
one in the index (both stay in the list).

**Not stored.** A buffer for content that is about to be saved.

**Writer.** Same order and primitives. `document_status` and
`acknowledge_status` are narrowed to one byte.

#### 3.12.1 Content file

*(not confirmed)* The content of a document is a file of its own in the
account's directory:

```
Acct<account oid>/<document oid><ext>
```

with `<document oid>` in decimal and `<ext>` chosen by `document_type`:

| `document_type` | `<ext>` |
|---|---|
| 1 | `.txt` |
| 3, 5 | `.pdf` |
| anything else | `.bin` |

The application reads it as an opaque byte stream and copies it out unchanged
when the user exports the document (for type 1 it can alternatively render a
readable preview). `content_hash` is the RIPEMD-160 digest of those bytes. The
application's clean-up treats any file in the account directory whose name is
digits, a dot and lower-case letters, and that no document record accounts
for, as an orphan and deletes it.

Presumably only records with `document_status` 0 have a content file; this
is not confirmed.

The content file is an entry of the vault, found by its name in the account's
directory. Neither that name nor the record's `file_name` is ever a path on
the host: a reader must not open, test or create anything in the host file
system on behalf of a name stored in a vault.

#### 3.12.2 Default title

After reading, `title` is compared with the title the application would
generate for the record, and replaced by "no value" if they are equal. The
comparison is exact: a title that differs by one trailing blank is kept. The
generated title is a localised name of the `document_type`, followed by a
blank and either the statement number (five digits, prefixed by the year and
`/` when a year is present) or the document date in the short date format of
the user's locale; when the record has neither, it is the name alone. With
German settings the generated titles look like `Kontoauszug 2024/00007`,
`Kontoauszug 00012` and `Bankdokument 31.07.2024`. The title therefore depends
on the language and regional settings of the installation. A reader cannot and
need not reproduce this; keep `title` as stored.

### 3.13 Signature folder item

File `Acct<oid>/AcctPayChecks`, one file per bank account. An order that sits
in the bank's distributed signature folder (EBICS distributed electronic
signature) and can be signed or cancelled from the application. Document 03
lists it as "pay check", after the file name. It belongs to
the payment family of document 04: it has the object header, then the payment
base, then its own fields in a second chunk.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `u8` | `class_and_direction` | Bits 4..7: `scheme_class`. Bit 0: `debit_flag`. Bits 1..3 unused. | See 3.13.2 for what the item keeps of it. |
| 4 | 1 | `decimal` | `amount` | Amount, unsigned; direction is in the debit flag. |  |
| 5 | 1 | `code(3)` | `currency` | ISO 4217 currency code. |  |
| 6 | 1 | `string` | `counterparty_name` | Name of the payee or payer. |  |
| 7 | 1 | `string` | `counterparty_account` | Account identifier of the counterparty (IBAN or national account number). |  |
| 8 | 1 | `string` | `counterparty_bank_code` | Bank identifier of the counterparty (BIC or national bank code). |  |
| 9 | 1 | `code(2)` | `counterparty_country` | ISO 3166 country code. |  |
| 10 | 1 | `string` | `purpose_text` | Remittance text, lines separated by LF. | Split into lines as in 04 section 5.6. |
| 11 | 1 | `date_raw` | `scheduled_date` | Requested execution date. |  |
| 12 | 1 | `string` | `order_id` | Order reference. |  |
| 13 | 1 | `string` | `category` | Category full name (section 4). | Normalised on read (4.2). |
| | 1 | *test* | | Test "more chunks follow" on chunk 1. If clear, the payment base ends here and position 14 onwards is not on the wire (legacy layout, 3.13.1). | Does not move the read position. |
| 14 | 1 | `string` | `counterparty_street` | Street line of the counterparty address. |  |
| 15 | 1 | `string` | `counterparty_town` | Town line of the counterparty address. |  |
| 16 | 1 | `string` | `end_to_end_id` | SEPA end-to-end id. |  |
| 17 | 1 | `string` | `payment_info_id` | SEPA payment information id (batch reference). |  |
| 18 | 1 | `string` | `purpose_code` | SEPA purpose code. |  |
| 19 | 1 | `string` | `category_purpose_code` | SEPA category purpose code. |  |
| 20 | 1 | `string` | `mandate_id` | SEPA mandate reference. |  |
| 21 | 1 | `date_raw` | `mandate_signed_date` | Date the mandate was signed. |  |
| 22 | 1 | `string` | `creditor_id` | SEPA creditor identifier. |  |
| 23 | 1 | `string` | `local_instrument_code` | SEPA local instrument code (for example CORE, B2B, INST). |  |
| 24 | 1 | `string` | `sequence_type` | SEPA direct debit sequence type. |  |
| 25 | 1 | `string` | `ultimate_counterparty_name` | Ultimate creditor or debtor name. |  |
| 26 | 1 | `u8` | `cost_bearer` | Enum `cost_bearer` (04 section 11.3). |  |
| 27 | 1 | `i32` | `cost_bearer_account_oid` | `oid` of the account charged for the fees, 0 if none. |  |
| 28 | 1 | `string` | `intl_remittance_method` | Payment method code of a foreign payment. |  |
| 29 | 1 | `string` | `intl_remittance_instruction` | Instruction text of a foreign payment. |  |
| 30 | 1 | `string` | `counterparty_bank_name` | Name of the counterparty bank. |  |
| 31 | 1 | `string` | `counterparty_bank_street` | Street line of the counterparty bank. |  |
| 32 | 1 | `string` | `counterparty_bank_town` | Town line of the counterparty bank. |  |
| 33 | 1 | `i32` | `scheme_type` | Enum `scheme_type` (04 section 11.2). | Read, then always replaced by 0 for this record type (3.13.2). |
| 34 | 1 | `time_hms` | `scheduled_time` | Requested execution time (instant payments). |  |
| 35 | 1 | `string` | `regulatory_reporting_code` | Regulatory reporting purpose code. |  |
| | 1→2 | *go to next chunk* | | Result is ignored. | A no-op in a single-chunk record (3.13.1). |
| 36 | 2 | `i32` | `item_state` | Enum `signature_folder_state`. |  |
| 37 | 2 | `i32` | `outbox_oid` | `oid` of a related outbox order (04 section 10), 0 if none. |  |
| 38 | 2 | `string` | `order_details_xml` | XML text with the order details of the item. | The reader parses it at once and fails on "no value" or malformed XML. See 3.13.3. |
| 39 | 2 | `u8` | `item_flags` | Flag byte. Banking4 defines only bit 0. | Meaning of bit 0 not established (open question 5). |
| 40 | 2 | `blob` | `order_data` | Raw order data. | Content not known. |
| 41 | 2 | `bool` | `vop_opt_in_preferred` | Verification-of-payee opt-in preference for this order. |  |

Positions 3 to 35 are the payment base, read by the base reader before any
field of the item itself. Document 04 section 5 is the reference for them
(there they are numbered from 1); the names are the same.

**After reading.** See 3.13.2.

**Loader.** Every record is kept. None of the adjustments that the payment list
applies to its records (04 section 8.3) apply here.

**Writer.** Same order and primitives, with these differences:

- A current writer always produces both chunks.
- `class_and_direction` is computed, not copied (3.13.2).
- `order_details_xml` is regenerated from the parsed order details, so it is
  not byte-identical to what was read.

#### 3.13.1 Legacy single-chunk form

As for the other payment records (04 section 5.2), the base reader tests after
`category` whether chunk 1 has a successor.

- **It has** (everything a current version writes): positions 14 to 35 are read
  from the rest of chunk 1, then the reader moves to chunk 2 for positions 36
  to 41.
- **It has not:** positions 14 to 35 are not on the wire. The purpose lines are
  scanned for tagged lines that fill some of those fields (04 section 5.8).
  The step to the next chunk does nothing, and positions 36 to 41 are read from
  chunk 1 directly after `category`.

#### 3.13.2 Scheme and direction

For this record type the base reader always ends by setting `scheme_type` to 0,
whatever position 33 contained, and it never derives a scheme from the class
nibble. Consequently:

- the direction of the item is the `debit_flag` bit of position 3 (set means
  money leaves the account);
- the class nibble carries no information a reader needs.

On writing, the byte is rebuilt as `scheme_class << 4 | debit`, where
`scheme_class` follows from the in-memory `scheme_type` (04 section 11.2) and
`debit` is the effective direction. For an item that was read from a file the
scheme is 0, so the byte is `00` or `01`.

#### 3.13.3 Order details and order data

`order_details_xml` is an XML document. Banking4 loads it and maps its root
element onto an order details structure of the EBICS protocol, reading it as
protocol version H005; "no value" or text that is not well-formed XML makes
the reader fail for that file. A default, empty structure is written like
this:

```
<OrderDetails xmlns="urn:org:ebics:H005"><OrderDataAvailable>false</OrderDataAvailable><OrderDataSize>0</OrderDataSize><OrderDetailsAvailable>false</OrderDetailsAvailable><TotalOrders>0</TotalOrders><TotalAmount>0</TotalAmount></OrderDetails>
```

The application apparently takes the item's order type, number of orders,
signatures done and required, originator and first signer from this
structure; that is not confirmed. The element set is that of the public EBICS
schema and is not described further here. A reader should store the text as it
is.

`order_data` is kept as opaque bytes.

### 3.14 Signature folder split

File `Acct<oid>/AcctPayCheckSplits`, one file per bank account. One record per
single transaction contained in a signature folder item, linked to it by
`item_oid`. A separate file from the items.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `i32` | `item_oid` | `oid` of the signature folder item this line belongs to. |  |
| 4 | 1 | `u8` | `split_flags` | Bit 0: debit flag. Other bits unused. | Writer emits 0 or 1. |
| 5 | 1 | `decimal` | `amount` | Amount of the single transaction. |  |
| 6 | 1 | `code(3)` | `currency` | ISO 4217 currency code. |  |
| 7 | 1 | `string` | `counterparty_name` | Name of the counterparty. |  |
| 8 | 1 | `string` | `counterparty_account` | Account identifier of the counterparty. |  |
| 9 | 1 | `string` | `counterparty_bank_code` | Bank identifier of the counterparty. |  |
| 10 | 1 | `string` | `purpose` | Remittance text. | Reader replaces every LF by a space. |
| 11 | 1 | `u8` | `vop_status` | Enum `vop_status`. |  |
| 12 | 1 | `string` | `vop_matched_name` | Name reported back by verification of payee. |  |
| 13 | 1 | `i32` | `position` | Position of the line within the item. |  |
| 14 | 1 | `string` | `end_to_end_id` | SEPA end-to-end id. |  |

**After reading.** In `purpose`, every LF is replaced by a space. Only bit 0 of
`split_flags` is used.

**Loader.** Every record is kept. A split whose `item_oid` matches no item is
removed only by the application's repair function, not on load.

**Writer.** Same order and primitives. `split_flags` is written as 0 or 1.

### 3.15 Invoice

File `Invoices`, global. An invoice issued or received by the user, with its
payment and dunning state.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `u8` | `invoice_type` | Enum `invoice_type`. |  |
| 4 | 1 | `i32` | `invoice_state` | Enum `invoice_state`. |  |
| 5 | 1 | `string` | `counterparty_name` | Name of the customer or supplier. |  |
| 6 | 1 | `string` | `customer_number` | Customer number. |  |
| 7 | 1 | `string` | `invoice_number` | Invoice number. |  |
| 8 | 1 | `ticks` | `invoice_date` | Invoice date. |  |
| 9 | 1 | `decimal` | `invoice_amount` | Invoice total. |  |
| 10 | 1 | `code(3)` | `currency` | ISO 4217 currency code. |  |
| 11 | 1 | `ticks` | `due_date` | Due date. |  |
| 12 | 1 | `string` | `category` | Category full name (section 4). | Unlike in the other records, the reader does not normalise it. |
| 13 | 1 | `ticks` | `dunning_date` | Date of the last dunning step. |  |
| 14 | 1 | `u8` | `dunning_level` | Enum `dunning_level`. |  |
| 15 | 1 | `decimal` | `dunning_charge` | Dunning charge. |  |
| 16 | 1 | `decimal` | `dunning_interest_percent` | Default interest rate in percent. |  |
| 17 | 1 | `decimal` | `amount_paid` | Amount already paid. |  |
| 18 | 1 | `string` | `notes` | Free user notes. |  |

**After reading, loader.** Nothing; every record is kept. Note that `category`
is the only category field in this document that is **not** normalised on
read.

**Writer.** Same order and primitives. `invoice_type` and `dunning_level` are
narrowed to one byte.

### 3.16 Online trace

File `OnlineTraces`, global. The catalogue of recorded bank communication
traces (the "trace entry" of document 03). `b4` never reads this file (03,
"Entries"); the layout is documentation.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `ticks` | `timestamp` | Time the trace was recorded. |  |
| 4 | 1 | `string` | `title` | Title of the trace. |  |
| 5 | 1 | `string` | `file_name` | Name of the trace file. | The file is outside the vault (3.16.1). |

**After reading.** The in-memory `trace_type` (section 6) is set to 0. It is
not on the wire.

**Loader.** See 3.16.1.

**Writer.** No difference.

#### 3.16.1 The trace file is not in the vault

`file_name` names a file in a trace directory on the computer's ordinary file
system. The loader drops every record whose `file_name` is "no value" or whose
file does not exist there. *(not confirmed)* So the list the application
shows depends on the machine; the vault only holds the catalogue.

A reader must not imitate that test. `file_name` comes from the vault and can
be any text, an absolute path or one with `..` included; looking it up would
let a vault probe which files exist on the machine that runs the reader. The
rule is general: a reader never touches the host file system on behalf of a
name stored in a vault. `b4` goes further for this file and does not read
`OnlineTraces` at all.

### 3.17 Import/export log entry

File `ImportExportPaymtsLog`, global. One entry per file of payments that was
imported or exported. Judging by the result codes, it serves to recognise a
file that was already processed.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id (stamp, see 02 section 5.1). | Other records refer to it. |
| 2 | 1 | `i32` | `update_stamp` | Update stamp (see 02 section 5.1). | -1 means not stamped yet. |
| 3 | 1 | `string` | `data_id` | Identifier of the imported or exported data. | How it is derived is not known (open question 8). |
| 4 | 1 | `i32` | `data_size` | Size of that data in bytes. |  |
| 5 | 1 | `ticks` | `timestamp` | Time of the import or export. |  |
| 6 | 1 | `u8` | `result` | Enum `import_export_result`. | The "uninitialised" member (-1) is written as byte 255 and reads back as 255. |

**After reading, loader.** Nothing; every record is kept.

**Writer.** No difference in order. `result` is narrowed to one byte.

## 4. Categories

### 4.1 Hierarchy

Categories form a tree, but the file stores no tree: there are **no parent
ids**. A category is identified by its **full name**, the names of all its
levels from the top down joined by a colon:

```
Home:Rent
Home:Utilities:Electricity
```

- The separator is the single character `:` (U+003A). A level name cannot
  contain it.
- The **name** of a category is the text after the last `:` (the whole text if
  there is none).
- The **parent** of a category is the text before the last `:`; a full name
  without `:` is a top-level category and has no parent.
- A child's full name is parent full name, `:`, child name.
- **Descendant test:** B belongs to A if B equals A, or B starts with A
  followed by `:`. This test is exact and case-sensitive.
- The depth of a category is the number of `:` in its full name.

The parent of a category is found by looking up the parent's full name in the
list. Nothing creates records for missing ancestors on load *(not
confirmed)*, so `Home:Rent` can exist without a `Home` record. A reader
should derive the tree from the names and be prepared for gaps.

`Categories` is a flat list of category records. In memory it is kept sorted
by full name with this ordering:

1. "no value" sorts before everything;
2. names are compared character by character after upper-casing each
   character, so the ordering **ignores case**;
3. at the first position where they differ, `:` sorts before every other
   character (a category is followed immediately by its descendants);
4. otherwise the upper-cased characters are compared by code unit;
5. if one name is a prefix of the other, the shorter sorts first.

Two full names are the **same category** when this comparison finds no
difference, that is, when they are equal ignoring case.

"Upper-casing each character" is meant literally, and a reader that builds a
matching key has to do the same: each character is replaced by its simple
upper-case form on its own, and the number of characters never changes. A character whose upper-case form would be more
than one character stays as it is. `ß` is therefore not turned into `SS`:
`Straße` and `Strasse` are two categories. A routine that upper-cases a whole
string with the full Unicode mappings gets this wrong.

### 4.2 Normalisation of stored names

Every category name in this document except the invoice's goes through the
normalisation of 04 section 5.7 when it is read. In short: "no value" stays;
the text is trimmed; it is then split at every `:`, each level is trimmed, an
empty level is replaced by the literal `UNBEKANNT`, and the levels are joined
with `:` again. Two corner cases involve localised user interface texts: a text
of white space only becomes the localised full name of the "do not evaluate"
category, and a text equal to the localised name of one of the pseudo
categories (4.3) becomes "no value". The localised names are listed in 04.

Examples: `" Home : Rent "` becomes `Home:Rent`; `Home::Rent` becomes
`Home:UNBEKANNT:Rent`; `:` becomes `UNBEKANNT:UNBEKANNT`.

### 4.3 Special and pseudo categories

**Special categories** are real records in `Categories`, marked by
`special_category`:

| Value | Role | Forced properties |
|---|---|---|
| 1 | reclassification | top level, `totals_bucket` 1, no budget, icon `reclass` (3.3.1) |
| 2 | excluded ("do not evaluate") | top level, `totals_bucket` 1, no budget, icon `exclude` (3.3.1) |
| 3 | VAT | none on read; created with icon `vat` and `totals_bucket` 0 |

There is at most one of each (3.3, loader). Their full names are ordinary,
localised texts chosen when they are created, and a booking refers to them by
that name like to any other category. The names the application gives them
in the Banking4 version described here:

| Value | German (default) | English | French |
|---|---|---|---|
| 1 reclassification | `(Umbuchung)` | `(Transfer)` | `(Transfert)` |
| 2 excluded | `(nicht auswerten)` | `(exclude)` | `(ne pas évaluer)` |
| 3 VAT | `(Umsatzsteuer)` | `(VAT)` | `(VAT)` |

The name is fixed when the category is created and a user can rename it
afterwards, so a reader recognises these categories by `special_category`,
never by name. The table is useful in the other direction: a booking whose
`category` is one of these texts while `Categories` holds no such record
refers to a special category of a catalogue that has not been created yet. If the catalogue has categories but lacks
one of the three, the application creates it after loading; if a category with
the intended name already exists, that one is converted. The VAT category is
not created by builds without VAT support. *(not confirmed)*

A category with `special_category` 0 is a **plain** category; the application
displays the others greyed out. Categories with `totals_bucket` 1 are left out
of totals, which is what the reader forces for the special categories 1 and
2.

**Pseudo categories** are not stored anywhere. They are in-memory placeholders
that the application adds to category pickers: the catalogue root, "not set"
and "automatic". Every category read from the file has `pseudo_category` 0.
They matter to a reader for one reason only: their localised display names must
never be taken for real category names (4.2).

### 4.4 Budgets

A budget is stored on the category itself, as two fields:

- `budget_amount`: the amount for one period;
- `budget_period`: the length of that period (enum `budget_period`).

There is no separate budget file and no per-month budget table.

The application derives *(not confirmed)*:

- **months per period:** 3 for `quarter`, 12 for `year`, 1 for everything
  else;
- **monthly budget** = `budget_amount` / months per period;
- **budget for a span of n months** = `budget_amount` × n / months per period,
  except for `budget_period` `all` (5), where it is `budget_amount` itself
  regardless of n;
- optionally plus the same quantity of all child categories, recursively.

A new category starts with `budget_period` `month` and amount 0; an amount of
0 presumably means "no budget". The special categories 1 and 2 never have a
budget. Which sign convention the amount of an expense budget uses was not
established (open question 6).

### 4.5 Rules

`rules` holds the keyword expression the application uses to suggest a
category for a booking. The reader parses it at once; a reader that only wants
to show the catalogue can keep the text.

The parsing and the canonical text below are confirmed (8.2).

**Structure.** The text is a list of **rules** separated by separator
characters. A rule is one or more **keywords** joined by `&`; apparently all
keywords of a rule must be present (the matching is not confirmed, open
question 7). A rule whose first character is `~` is a negative rule.

**Characters.** The parser goes through the text character by character and
sorts each one into one of four classes:

| Class | Characters | Effect |
|---|---|---|
| marker | `&`, `~` and `#` (for `layout_version` 4 and above; `+`, `-` and `#` below, see "Version dependence") | taken as they are |
| keyword character | the digits, `a` to `z`, `ä`, `ö`, `ü`, `ß`, and `*` `+` `,` `-` `.` `/` `?` | taken as they are |
| folded | `A` to `Z`, `Ä`, `Ö`, `Ü` | replaced by the lower-case letter |
| separator | everything else: white space, control characters, all other punctuation, all other letters of Latin-1 (`é`, `à`, `ñ`, ...), and every character above U+00FF | ends the current piece |

So only the 26 letters and the German umlauts are letters for this purpose; a
word such as `café` is cut after `caf`.

**From pieces to rules.**

1. A piece is a run of characters between separators. Pieces shorter than two
   characters are dropped.
2. If the piece starts with `~`, the rule is negative and that one character
   is removed. A `~` anywhere else stays in the text as part of a keyword.
3. The rest is cut at every `&`. Keywords shorter than two characters are
   dropped; a rule left without a keyword is dropped. (`x&flat` is the rule
   `flat`; `~q` is nothing.)
4. Within a rule the keywords are sorted. The positive rules are sorted, and
   so are the negative rules. Duplicate rules are kept.

"No value" and a text without any rule both give "no rules".

**Canonical form**, as the writer produces it: the positive rules joined by
one space; then, if there are negative rules, one space followed by the
negative rules joined by one space, each prefixed by `~`; within a rule the
keywords joined by `&`. Example: `Rent&FLAT landlord ~Janitor` is written
back as `flat&rent landlord ~janitor`. Four details:

- With negative rules only, the text **starts with a space**:
  `~janitor ~cleaning&stairs` is written back as ` ~cleaning&stairs ~janitor`.
- No rules are written as "no value".
- **The sort order is not fixed.** Keywords and rules are ordered by
  Banking4's ordinary string comparison, which follows the language settings
  the application runs under, not the code points. Under German settings
  `ü` sorts with `u` (`nt ub üc ud xz`), `ß` comes before `z`
  (`ßx zd`), and `-a ?a 1a a1` comes out as `?a 1a -a a1`. Rules are compared
  keyword by keyword, and a rule that is the beginning of another one comes
  first (`ab`, `ab&cd`, `abc`). For keywords made of `a` to `z` only, the
  order is plain alphabetical. A reader must not rely on the order, and must
  not expect to reproduce the canonical text byte for byte.
- The form is ambiguous in one corner: a positive rule whose first keyword in
  sort order starts with `~` (possible because of step 2) is written with a
  leading `~` and is a negative rule when it is read again.

**Version dependence.** When `layout_version` is below 4, the joining marker
is `+` instead of `&`, and `&` and `~` are separators (the markers are then
`+`, `-` and `#`; `+` and `-` are keyword characters anyway).
So the version 3 text `rent+flat landlord` is written back as
`flat&rent landlord`. A leading `-`, apparently the negative marker of those
versions, is **not** converted: `-janitor` stays a positive rule with the
keyword `-janitor`, and a text of those versions cannot express a negative
rule at all.

Three characters are apparently markers for the matching: `#` (a digit), `?`
(any character) and `*` (a wildcard). How a rule is matched against the text
of a booking, including the meaning of these three, is not known (open
question 7).

### 4.6 How other records refer to a category

By **full name, as text**. There is no category id anywhere:

- a booking, payment or standing order carries the text in the `category` field
  of the payment base (04 section 5.1);
- payee, payer, mandate and invoice records carry a `category` field with the
  default category for bookings that match them.

To find the category record for such a text, normalise the text (4.2) and look
for the record whose `full_name` is equal ignoring case (4.1). The lookup the
application uses does not trim or otherwise alter either side.

A text may have **no** matching record: the application then creates a plain
category of that name when it needs one *(not confirmed)*. A reader should
therefore take the union of the names in `Categories` and the names used by
records, and left-join the catalogue attributes.

Because there is no indirection, renaming or moving a category means rewriting
the text in every record that refers to it; the application has routines that
do this over whole collections *(not confirmed)*.

## 5. Portfolio data

### 5.1 Security identifiers

Securities are identified by text. Three records carry such a text: the stock
instrument, the portfolio item and the portfolio transaction (field `isin` in
each).

The application interprets the text in these steps. All tests are
case-sensitive.

1. The text is trimmed of white space at both ends. "No value" or an empty
   result is **no identifier**.
2. If the text now starts with `ISIN` and one space, those five characters are
   removed. Nothing is trimmed again, and the following steps apply to what is
   left, whatever it is (`ISIN WKN 123456` is handled as `WKN 123456`).
3. The first matching row of this table applies:

| Text | Interpretation | Number part | Canonical ISIN |
|---|---|---|---|
| a valid ISIN (see below) | ISIN | | the 12 characters |
| starts with `WKN` and one space | national number, country `DE` | the text after those four characters | see below |
| starts with `WKNR` and one space | national number, country `DE` | the text after those five characters | see below |
| exactly six characters, each an upper-case letter `A` to `Z` or a digit | national number, country `DE` | the six characters | see below |
| starts with `/TS/` | ticker symbol | | none |
| starts with `/XX/` | bilaterally agreed identifier | | none |
| longer than four characters, with `/` as first and fourth character and two upper-case letters `A` to `Z` between them | national number of that country | the text after the fourth character | only for `DE`, see below |
| anything else | no identifier | | none |

`/TS/` and `/XX/` need nothing after them. The country form needs at least one
character after the second `/`, and its two letters are not checked against a
list of countries (`/ZZ/1` is a national number).

A **valid ISIN** is 12 characters: two upper-case letters, nine upper-case
letters or digits, one digit, and the check digit must be right. The check:
replace every letter by its two-digit value (A is 10, Z is 35) and apply the
Luhn test to the resulting digit string, that is: going from the last digit
of the expanded first eleven characters to the first, double every second
digit starting with the last one, add up the decimal digits of all results,
and take the amount missing to the next multiple of ten. A lower-case
spelling or a wrong check digit is not an ISIN.

A **national number** has a canonical ISIN only when its country is `DE` and
its number part is exactly six characters long: `DE000`, the six characters,
and the check digit computed by the same rule. Example: `123456`,
`WKN 123456` and `/DE/123456` all give `DE0001234565`. A number part of any
other length, and a national number of another country, has none.

The number part is not validated when the text is interpreted, only its
length counts. Two consequences for six-character number parts that reach
this point through one of the prefixed forms:

- Lower-case letters count like upper-case ones in the check digit, and are
  copied as they are: `WKN abcdef` gives `DE000abcdef2`, which is not a valid
  ISIN when it is read again.
- Any character other than a letter `A` to `Z`, `a` to `z` or a digit (a
  blank, a hyphen, an umlaut) makes Banking4's derivation of the canonical
  ISIN fail with an error. Interpreting the text does not fail, so Banking4
  accepts such a stock instrument on reading but cannot write it (3.6). A
  reader should treat it as having no canonical ISIN.

### 5.2 How the records relate

```
portfolio item (Acct<oid>/Portf)             stock instrument (StockInstruments)        stock price (Stock<oid>/StockPrices)
  isin (text) ── canonical ISIN ──────────▶   isin
                                               oid ─────── directory name ───────────▶   one record per day
portfolio transaction (Acct<oid>/PortfTxns)
  isin (text)
```

- **Item to instrument: by ISIN, not by id.** The item's `isin` text is
  interpreted as in 5.1; if that yields a canonical ISIN, the instrument with
  exactly that ISIN is the item's instrument. An item whose identifier has no
  canonical ISIN cannot be matched to an instrument by a reader.
- **Instrument to price series: by object id.** The series of an instrument is
  the file `Stock<oid>/StockPrices` with the instrument's `oid`. Price records
  contain neither an ISIN nor an instrument id; the directory name is the only
  link.
- **Transactions** carry the same kind of `isin` text and can be matched to
  items and instruments the same way. They have no id link either.
- Several items, in different accounts, can share one instrument.

How the instrument list is maintained *(not confirmed)*:

- Whenever an item has a `current_price` other than 0, a `current_price_date`
  and a `price_type` other than 0, the application looks up its instrument,
  creating it (with the item's `name` and `current_price_currency`) if needed,
  and inserts or updates the price for that date in the series. An item with
  `price_type` 1 and no `current_price_currency` does not get an instrument
  created.
- If the item's `current_price_currency` differs from the instrument's
  `price_currency`, the whole series is discarded and the instrument's currency
  replaced.
- The vault clean-up removes every instrument whose ISIN does not equal the
  stored `isin` text of at least one portfolio item of some portfolio
  account.

One consequence of these rules: for an item whose identifier is recognised
but has no canonical ISIN (a ticker symbol, a bilateral identifier, a foreign
national number), the lookup finds nothing and the application may still
create an instrument. That instrument is written with `isin` "no value", so
after the next load it is not in the ISIN index and nothing refers to it.
`StockInstruments` can therefore contain records without an ISIN, each with
its own `Stock<oid>/StockPrices`, that cannot be attributed to any item. A
reader should tolerate them.

So `StockInstruments` is essentially "the securities currently held, with
their price history as collected by this installation", and the prices are in
the instrument's `price_currency`.

### 5.3 Quantity and price types

`quantity_type` says what `quantity` counts, `price_type` says what the two
prices mean (values in section 6):

| `price_type` | Value of the position for a given price | Currency of that value |
|---|---|---|
| 1 amount per unit | `price × quantity` | the price's currency |
| 2 percent | `price × quantity / 100` | `quantity_currency` |
| 0, 3 | not computed (0) | |

The result is rounded to two decimal places, halves away from zero, and a
price that is not greater than 0 gives 0. *(not confirmed)*

- The **current value** uses `current_price`.
- The **purchase value** uses `purchase_price`, first adjusted for stock splits
  (3.6.1) between `purchase_price_date` and today when the item has an
  instrument and a purchase date.

`quantity_type` does not enter the formula. It decides whether
`quantity_currency` is meaningful: for `units` (2) the reader discards it
(3.8.1); for `face_amount` (1) the quantity is a nominal amount in that
currency, the usual combination with `price_type` 2 for bonds.

## 6. Enumerations

Names are ours. "Flags" marks a bit set; "alias" marks a name for a
combination or a duplicate of other values. Where a note says "apparently",
the meaning is not confirmed. Enumerations of the payment base
(`scheme_class`, `scheme_type`, `cost_bearer`) are in 04 section 11.

### 6.1 `payee_kind` (`u16`)

The values are single bits like those of a flag set, but Banking4 uses one
value per record. The reader accepts only 0, 1, 2 and 4.

| Value | Name | Notes |
|---|---|---|
| `0x0000` | `none` |  |
| `0x0001` | `payee` | ordinary entry of the payee list |
| `0x0002` | `legacy_mandate` | apparently a legacy value |
| `0x0004` | `auto` | apparently an entry the application added on its own |
| `0x0008` | `account` | apparently an entry standing for one of the user's own accounts; never survives a read (replaced by 0, see 3.1) |
| `0xFFFF` | `all` | alias, mask of every kind |

### 6.2 `payee_flags` (`u16`, flags)

| Value | Name | Notes |
|---|---|---|
| `0x01` | `vop_no_match_unsupported` | the only member; apparently unsupported |

### 6.3 `vop_status` (`u8`)

Result of a verification of payee request. Used by payee and signature folder
split.

| Value | Name | Notes |
|---|---|---|
| 0 | (none) | no member has this value; it is what an unset field holds |
| 1 | `match` |  |
| 2 | `close_match` |  |
| 3 | `no_match` |  |
| 4 | `not_applicable` |  |
| 5 | `pending` |  |

### 6.4 `totals_bucket` (`u8`)

| Value | Name | Notes |
|---|---|---|
| 0 | `auto` |  |
| 1 | `exclude` | category is left out of totals |
| 2 | `income` |  |
| 3 | `expense` |  |

### 6.5 `budget_period` (`u8`)

The application uses the same enumeration for evaluation time ranges; the
notes give the meaning as a budget period (4.4).

| Value | Name | Notes |
|---|---|---|
| 0 | `none` | counted as 1 month |
| 1 | `week` | apparently retired; counted as 1 month |
| 2 | `month` | 1 month |
| 3 | `quarter` | 3 months |
| 4 | `year` | 12 months |
| 5 | `all` | the amount is a fixed total, not scaled by time |
| 6 | `month_range` | evaluation ranges only; as a budget period counted as 1 month |

### 6.6 `special_category` (`u8` on the wire, `i32` in memory)

| Value | Name | Notes |
|---|---|---|
| 0 | `none` | ordinary category |
| 1 | `reclassification` | apparently transfers between own accounts |
| 2 | `excluded` | "do not evaluate" |
| 3 | `vat` | value added tax |

### 6.7 `pseudo_category` (flags, never stored)

| Value | Name | Notes |
|---|---|---|
| `0x00` | `none` | every category read from a file |
| `0x01` | `root` | the catalogue root |
| `0x02` | `not_set` | "not set" |
| `0x04` | `automatic` | "automatic" |

### 6.8 `mandate_flags` (`i32`, flags)

| Value | Name | Notes |
|---|---|---|
| `0x0000` | `none` |  |
| `0x0001` | `hidden` |  |
| `0x0100` | `revoked` |  |

### 6.9 `portfolio_item_state` (`i32`, flags)

| Value | Name | Notes |
|---|---|---|
| `0x00000` | `none` |  |
| `0x00001` | `local` |  |
| `0x00002` | `bank` |  |
| `0x0FFFF` | `all` | alias, mask of the origin bits |
| `0x10000` | `deleted` |  |

### 6.10 `quantity_type` (`i32` in the portfolio item, `u8` in the portfolio transaction)

The members correspond to the ISO 15022 quantity type codes.

| Value | Name | Notes |
|---|---|---|
| 0 | `undefined` |  |
| 1 | `face_amount` | code `FAMT`: quantity is a nominal amount in `quantity_currency` |
| 2 | `units` | code `UNIT`: quantity is a number of units |

### 6.11 `price_type` (`i32`)

The members correspond to the ISO 15022 price type codes.

| Value | Name | Notes |
|---|---|---|
| 0 | `undefined` |  |
| 1 | `actual` | code `ACTU`: price is an amount per unit |
| 2 | `percent` | code `PRCT`: price is a percentage of the face amount |
| 3 | `yield` | code `YIEL`: price is a yield; the application computes no value from it |

### 6.12 `portfolio_transaction_state` (`i32`)

| Value | Name | Notes |
|---|---|---|
| 0 | (none) | no member has this value |
| 1 | `pending` |  |
| 2 | `executed` |  |
| 3 | `settled` |  |

### 6.13 `delivery_direction` (`i32`)

The members correspond to ISO 15022 codes.

| Value | Name | Notes |
|---|---|---|
| 0 | (none) | no member has this value |
| 1 | `deliver` | code `DELI`: securities leave the portfolio |
| 2 | `receive` | code `RECE`: securities enter the portfolio |

### 6.14 `transaction_activity` (`i32`)

The members correspond to ISO 15022 codes.

| Value | Name | Notes |
|---|---|---|
| 0 | (none) | no member has this value |
| 1 | `borrowing_lending` | code `BOLE` |
| 2 | `collateral` | code `COLL` |
| 3 | `corporate_action` | code `CORP` |
| 4 | `settlement` | code `SETT`: ordinary trade settlement |

### 6.15 `document_type` (`i32`)

| Value | Name | Notes |
|---|---|---|
| 0 | `unknown` |  |
| 1 | `statement_mt940` | content file extension `.txt` |
| 2 | `statement_iso8583` |  |
| 3 | `statement_pdf` | content file extension `.pdf` |
| 4 | `user` |  |
| 5 | `bank_pdf` | content file extension `.pdf` |
| 6 | `empty` |  |
| 7 | `bank_swift` |  |
| 8 | `mail` |  |
| 9 | `mail_pdf` |  |
| 10 | `statement_camt053` |  |

### 6.16 `document_bits` (`u8`, flags)

| Value | Name | Notes |
|---|---|---|
| `0x01` | `read` |  |
| `0x02` | `exported` |  |

### 6.17 `document_status` (`u8` on the wire, `i32` in memory)

| Value | Name | Notes |
|---|---|---|
| 0 | `contained` |  |
| 1 | `download_pending` |  |
| 2 | `download_requested` |  |
| 3 | `download_failed` |  |
| 4 | `download_discarded` |  |
| 5 | `empty` |  |

### 6.18 `acknowledge_status` (`u8` on the wire, `i32` in memory)

| Value | Name | Notes |
|---|---|---|
| 0 | `not_acknowledged` |  |
| 1 | `acknowledged` |  |
| 2 | `acknowledge_failed` |  |
| 3 | `needs_consent` |  |

### 6.19 `signature_folder_state` (`i32`)

| Value | Name | Notes |
|---|---|---|
| 0 | `none` |  |
| 1 | `sign_in_outbox` |  |
| 2 | `sign_sent` |  |
| 3 | `sign_failed` |  |
| 4 | `cancel_in_outbox` |  |
| 5 | `cancel_sent` |  |
| 6 | `cancel_failed` |  |

### 6.20 `invoice_type` (`u8` on the wire, `i32` in memory)

| Value | Name | Notes |
|---|---|---|
| 0 | `receivable` |  |
| 1 | `payable` |  |

### 6.21 `invoice_state` (`i32`)

| Value | Name | Notes |
|---|---|---|
| 0 | `open` |  |
| 1 | `paid` |  |
| 2 | `reversed` |  |
| 3 | `uncollectable` |  |
| 4 | (count) | count sentinel, not a state |

### 6.22 `dunning_level` (`u8` on the wire, `i32` in memory)

| Value | Name | Notes |
|---|---|---|
| 0 | `none` |  |
| 1 | `demand_note` |  |
| 2 | `first_dunning` |  |
| 3 | `second_dunning` |  |
| 4 | `last_dunning` |  |
| 5 | `legal_dunning` |  |

### 6.23 `trace_type` (never stored)

| Value | Name | Notes |
|---|---|---|
| 0 | `online_trace` |  |
| 1 | `app_log` |  |

### 6.24 `import_export_result` (`u8` on the wire, `i32` in memory)

| Value | Name | Notes |
|---|---|---|
| -1 | `uninitialised` | in-memory value; stored as byte 255 and read back as 255, not as -1 |
| 0 | `success` |  |
| 1 | `cancelled` |  |
| 2 | `failure` |  |
| 3 | `bad_file` |  |
| 4 | `bad_account` |  |
| 5 | `duplicate` |  |
| 6 | `io_error` |  |
| 7 | `exception` |  |

### 6.25 `account_kind` (`u8`, flags; belongs to the account record)

Only the bits and the two masks that decide which files of this document the
application opens (section 1). The account document has the full set.

| Value | Name | Notes |
|---|---|---|
| `0x01` | `bank` |  |
| `0x02` | `cash` |  |
| `0x04` | `credit_card` |  |
| `0x08` | `portfolio` |  |
| `0x10` | `asset` |  |
| `0x20` | `paypal` |  |
| `0x40` | `crypto` |  |
| `0x7F` | `has_documents` | alias: every kind; the same value as the mask of all kinds |
| `0x01` | `has_signature_folder` | alias: same value as `bank` |

## 7. Reader and writer differences at a glance

| Record | Field | Reader | Writer |
|---|---|---|---|
| payee | position 17 | reads a `string`, discards it | always "no value" |
| payee | `address`, `bank_address` | splits at the first LF, trims both parts | joins two lines with LF; "no value" only if both are missing |
| payee | `payee_kind` | values other than 0, 1, 2, 4 become 0 | writes the in-memory value |
| category | `layout_version` | read, steers positions 6 and 9 | constant 9 |
| category | `rules` | parsed, version dependent | canonical text regenerated |
| category | positions 7, 15 | read and discarded | "no value" and 0 |
| category | `budget_period` | forced to 2 when `layout_version` < 6 | writes the in-memory value |
| category | special categories 1, 2 | `full_name`, `totals_bucket`, budget and icon forced | writes the in-memory values |
| stock instrument | `isin` | accepts several spellings (5.1) | canonical ISIN or "no value" |
| stock instrument | `splits` | parsed into a list | regenerated from the list |
| portfolio item | `name` | LF replaced by space | as held |
| portfolio item | five `decimal` fields | negative becomes 0 | as held |
| portfolio item | `quantity_currency`, `purchase_price_currency` | adjusted (3.8.1) | as held |
| portfolio transaction | `quantity_type`, `interest_days` | `u8`, `u16` | narrowed from 32 bits |
| asset | `signed_amount` | split into amount and debit flag | recombined |
| account document | `title` | "no value" if equal to the generated default | as held |
| signature folder item | `class_and_direction` | only bit 0 is kept | recomputed from scheme and direction |
| signature folder item | `scheme_type` | read, then set to 0 | writes the in-memory value |
| signature folder item | `order_details_xml` | parsed | regenerated |
| signature folder split | `split_flags` | bit 0 | 0 or 1 |
| signature folder split | `purpose` | LF replaced by space | as held |
| online trace | `trace_type` | not read, set to 0 | not written |
| import/export log entry | `result` | 0..255 | low byte, so -1 becomes 255 |
| all but invoice | `category` | normalised (4.2) | as held |
| all | text of lines | split as in 02 section 5.2 | joined with LF |

Records for which no row is listed (payer, mandate, exchange rate, stock price,
tax exemption, invoice) are read and written symmetrically, apart from the
generic primitive asymmetries of 02 section 6.

## 8. Verification

The twelve record types of sections 3.1 to 3.12 (payee, payer, category,
mandate, exchange rate, stock instrument, stock price, portfolio item,
portfolio transaction, tax exemption, asset, account document) are confirmed
on test files produced with Banking4: 8.1 to 8.4. For the other five
(signature folder item and split, invoice, online trace, import/export log
entry) the repository has no test files; 8.5 says what is confirmed for them
and for the hierarchy rules of 4.1. All of that concerns the Banking4 version
described here and synthetic data only. `b4`'s output has also been compared
with data exported from Banking4 for one real vault: 8.6.

### 8.1 Test files

1. `tests/fixtures/records/` holds the test files for the twelve record
   types: per record type one collection file (`<record>.bin`) and one
   description (`<record>.json`) that gives, for every record, the
   in-memory values it was built from (`written`) and the expected result
   of reading it (`read`). 209 records in all. The well-formed records were
   produced with Banking4. They cover every field with a value of its own,
   records with no field set, every member of every enumeration, empty and
   missing texts, text outside ASCII, timestamps marked as UTC or local
   time, and the special cases this document describes (addresses, category
   names, special categories, identifier forms, splits, the portfolio item
   adjustments, the debit flag of an asset, generated document titles). The
   other records are cases with their expected results: values outside the
   enumerations, values wider than their wire width, and partial and
   impossible dates. The descriptions also name the fields that exist in
   memory only; they do not reach the file.
2. The tests of the repository (`tests/records_masterdata.rs`) decode every
   record of these files with `b4` and compare the result with the
   description. Where `b4` deliberately differs from Banking4, the test
   names the difference.
3. The field names of data exported from Banking4 for a real vault were
   compared with the tables: names only, no values. Four of the twelve
   record types (payers, mandates, assets, account documents) are not in the
   Banking4 export used for comparison; of the other eight, the vault had
   records for seven.

Two notes on the test files. Their expected results assume the default
currency `EUR`, the country `DE` and the German language, and their
account-bound records belong to an account whose currency is `CHF`, so that
the two currencies can be told apart. And Banking4 replaces an update stamp
of -1 by the current time on writing, so the test files never contain that
value.

### 8.2 Confirmed

Confirmed on the test files of 8.1, for all twelve record types:

- **Layout.** Order, primitive and width of every position of the tables in
  3.1 to 3.12, in both directions; that each record is a single chunk; that
  nothing else is written; that the "After reading" rules give the values
  the descriptions list as the result of reading.
- **Narrowing.** Values wider than their wire width are cut to it by the
  writer (`payee_kind`, `payee_flags`, `vop_status`, `special_category`,
  `quantity_type` and `interest_days` of the portfolio transaction,
  `document_status`, `acknowledge_status`), and codes longer than their width
  are cut.
- **Timestamps.** `ticks` fields keep the time of day and lose the UTC or
  local marker.

Also confirmed for all twelve record types:

- **Update stamp.** The writer replaces -1 by the current time.
- **Older and newer records.** A record that ends early, at any byte, reads
  as described in 02 section 3: the fields that are missing take their
  defaults, a field that is cut in the middle too, and a smaller field after
  it may still read the bytes that are left. That holds whether the chunk
  that ends early is the end of the record, is followed by unused bytes of
  the record, or is followed by another chunk. Unknown bytes at the end of
  the chunk and unused bytes after it are ignored. A record that is followed
  by an unknown chunk still reads to the same values, and Banking4 then
  rejects the file with the compatibility error after that record (02
  section 2.4); `b4` deviates from the latter on purpose (02 section 8).
- **Content a current writer does not produce.** Filled unused slots, older
  layout versions, enumeration values that are no listed member, unusual
  category, address, identifier, split and rule texts, and surplus bytes
  read as this document describes.

Confirmed per record type:

- **Payee:** the address split and join of 3.1.1 with no, one and several
  LFs, blanks and empty lines; `payee_kind` filtering (0, 1, 2 and 4 kept;
  3, 8, `0xFFFF` and others become 0); position 17 read and discarded,
  written as "no value"; the name digest and the initials are not stored.
- **Payer, mandate:** category normalisation; the reading of a mandate
  record.
- **Category:** normalisation of `full_name` (4.2, German texts); the
  adjustments of 3.3.1 for the values 1, 2, 3 and others; `layout_version`
  written as 9; the version rule for `budget_period` (versions 0 to 5 against
  6 and above); the unused slots; `pseudo_category` is not stored; rule
  texts (4.5) with both marker sets, including the complete character
  classes.
- **Exchange rate:** the "fixed rate" flag and the currency class are not
  stored.
- **Stock instrument:** every row of 5.1 and the corner cases listed there;
  the writer stores the canonical ISIN or "no value" and fails where that
  cannot be derived; the split line format of 3.6.1 in both directions, with
  every row of its table.
- **Stock price:** the worked example of 3.7, byte for byte.
- **Portfolio item:** all five adjustments of 3.8.1, including that the
  fallback currency is the application's default and not the account's
  currency, and that `quantity` is not guarded.
- **Portfolio transaction, tax exemption:** no adjustments; negative
  `interest_days` and values above 65535 come back as their low 16 bits.
- **Asset:** the sign rule of 3.11.1 and its two side effects.
- **Account document:** the default title rule of 3.12.2 (German texts); a
  hash of any length is stored as it is.

Comparison of names with data exported from Banking4: every field name the
exported data has for payees, categories, exchange rates, stock instruments,
stock prices, portfolio items and tax exemptions maps to a field of the
corresponding table, to one of the in-memory fields this document lists as
not stored (which hold their defaults in every exported record), or to a key
that names the owning account or instrument. The enumeration values that
occur in payees, categories and portfolio items are all listed members.

### 8.3 Corrections made

1. **Canonical rule text (4.5).** With negative rules only the text starts
   with a space; no rules are "no value". Previously: all rules separated by
   one space.
2. **Order of rules and keywords (4.5).** It follows Banking4's
   language-dependent string comparison. Previously: "sorted", which reads as
   a fixed order.
3. **Rule characters (4.5).** The complete classes are now given. Previously
   the document said that Latin-1 letters such as `ü` and `ß` are keyword
   characters; in fact only `ä`, `ö`, `ü` and `ß` are, and every other
   accented letter is a separator. Digits, `.` and `/` were not mentioned.
   `#` is not a keyword character but a marker that is always let through.
4. **Negative marker (4.5).** `~` marks a negative rule only as the first
   character of a piece and is otherwise part of a keyword; the resulting
   ambiguity of the canonical text is described.
5. **Split lines (3.6.1).** The accepted and rejected forms are now exact:
   only `R` means reverse and everything else is an ordinary split; the
   factor may carry a sign and white space and may be zero or negative; text
   after the third `/` is ignored; an impossible date, a missing date or
   factor, and an empty line make the reader fail. Previously: only that a
   malformed line makes the reader fail.
6. **Identifiers (5.1).** The `ISIN` prefix is removed before all other
   tests, not only before the ISIN test; `/TS/` and `/XX/` need no text after
   them; the country form needs two upper-case letters and at least one more
   character; the number part of a national number is not validated, with the
   two consequences listed there (lower-case letters, failing derivation).
7. **Mandate (3.4).** The reading of a mandate record is now confirmed
   (8.2); the statement that it was not confirmed is gone.
8. **Asset (3.11.1).** Added that a zero amount loses its debit flag and that
   the writer does not look at the sign of the in-memory amount.
9. **Payee address (3.1.1).** Added what the writer does with empty lines and
   with a line break inside the street line.
10. **Default title (3.12.2).** Now confirmed; added that the comparison is
    exact, that the name stands alone when there is neither number nor date,
    and examples.

None of these changes a table of section 3: all positions, primitives and
field assignments of the twelve record types were right as written.

### 8.4 Not confirmed

- **Everything about collections rather than single records** is still not
  confirmed: the file names and conditions of section 1, all "Loader"
  paragraphs (the payee-to-payer migration, dropping of categories and
  mandates, the creation and de-duplication of special categories, the
  exchange rate inversions, the duplicate-ISIN failure, the document index),
  budget arithmetic, the content file naming of 3.12.1, the maintenance of
  the instrument list in 5.2, the value formula of 5.3 and the split
  adjustment. The test files of 8.1 show how single records are read and
  written, not what Banking4 does with a list when it loads it. One test
  file shows a little of that: the demo vault (`tests/fixtures/vault/`), a
  vault produced with Banking4 with well-formed content. Nothing in it is
  dropped, added or changed on loading, and the lists of 3.1, 3.5, 3.8 and
  4.1 keep their order. That agrees with the sort orders given there (on
  one well-ordered input each), and it shows that the special categories
  are not duplicated when all three exist. It shows none of the rules for
  content that is *not* well-formed, which is what the loader paragraphs
  are mostly about.
- **Editions.** The expected results of the test files of 8.1 are those of
  Banking4's default product variant, which is not the full edition
  (section 1): Banking4 loads payers, invoices and portfolio transactions
  only in the professional or the business edition. Reading and writing a
  single record does not depend on that. The expected results of the demo
  vault are those of the professional edition.
- **Languages other than German.** Category normalisation and the default
  title are confirmed with German texts only.
- **Rule order under other language settings, and rule matching** (open
  question 7).
- **Real data for five record types.** The Banking4 export used for
  comparison had no payers, mandates, assets and account documents, and the
  real vault that was compared (8.6) has no file of payers, assets or
  portfolio transactions. For these the layout is confirmed on test files
  produced with Banking4, but not, value by value, against a file a real
  installation wrote. For mandates there is a little more: the Rust decoder
  read the 53 records of a real `Mandates` file without an error, with
  nothing to compare the values with.
- **Raw bytes of a real vault** for the record types the Banking4 export
  used for comparison does not contain (the item above) and for the fields
  it does not show. For seven record types a real collection file decodes to
  the exported values (8.6).
- **Older program versions.** Everything here describes one build. What a
  current reader makes of a shorter record is known (8.2); which fields
  older versions really wrote is not (open question 12).
- **Other platforms.** A few details may differ between the platforms
  Banking4 runs on and are not confirmed for all of them (02, open
  question 8).

### 8.5 Record types without test files

The repository has no test files for the five record types of 3.13 to 3.17
(signature folder item and split, invoice, online trace, import/export log
entry). Less is confirmed for them than for the other twelve, and with
synthetic data only:

- The tables of 3.13 to 3.17 are confirmed on records produced with
  Banking4 that hold a distinct value in every field: every position holds
  the value of the field the table assigns to it, no bytes are left over,
  and each record has one chunk, the signature folder item two.
- Reading such a record is confirmed as well: every field comes back with
  the value it was written from, except those listed in section 7.
- Also confirmed: bit 0 of `split_flags`; byte 255 for
  `import_export_result`; both chunk forms of the signature folder item
  (3.13.1), including a tagged purpose line filling `end_to_end_id`; that
  `scheme_type` is reset; that a third chunk is left unread with its flag
  still set.
- The default XML in 3.13.3 is confirmed: Banking4 writes it and accepts it.

Not confirmed for these five record types, apart from the cases just listed:
how Banking4 reads a record that ends early, and how it reads values that a
current writer does not produce.

The rules of 4.1 are confirmed: ordering ignoring case and ranking `:`
first, last level, parent name, joining, and the case-sensitive descendant
test.

### 8.6 Compared with a real vault

`b4`'s output for one real vault, written by a newer program version than the
Banking4 version described here, was compared value by value with data
exported from Banking4 for the same vault. No differences were found:

| Record | Records compared |
|---|---|
| category | 51 |
| payee | 455 |
| exchange rate | 51 |
| stock instrument | 43 |
| stock price | 10,419 |
| portfolio item | 5 |
| tax exemption | 12 |

What the records of that vault show beyond the tables of section 3:

- **Payee** and **portfolio item** records each carry 2 unread bytes after
  the last known field, zero in every record. What they are is unknown (open
  question 12); a reader skips them (02 section 3).
- **Category** records store `layout_version` 8, not the 9 that the Banking4
  version described here writes, and end one byte before the table of 3.3 does:
  position 15, `reserved_byte`, is missing and reads as its default.
- A `Mandates` file with 53 records exists. The Banking4 export used for
  comparison had no mandates, so their values have no cross-check (8.4).
- There are no `Payers`, `PortfTxns` and `Assets` files in it.

The comparison covers what the exported data contains: the values of the
records after reading, without the in-memory fields and without fields the
export leaves out. It is one vault of one program version, with well-formed
content; it confirms none of the loader rules for dropped or changed records
(8.4).

## 9. Open questions

1. **UTC or local time in `ticks` fields** (`quote_time`, `mandate_date`,
   `valid_from`, `valid_until`, `settlement_date`, `trading_date`,
   `document_date`, invoice dates, log and trace timestamps). The encoding
   drops the marker (02, open question 1); which convention each field follows
   was not established. Several of them are probably plain dates at midnight.
2. **`mandate_date`** of the mandate: which date it is (signature, first or
   last use).
3. **`portfolio_key`** of the portfolio item: what the key identifies.
4. **`interest_method`** of the asset: the set of one-character codes.
5. **Signature folder item:** the meaning of bit 0 of `item_flags` (it is
   handed on to the outbox order created for signing or cancelling), the
   content of `order_data`, and the element set of `order_details_xml` beyond
   what the public EBICS schema says. Also whether `amount` and the
   counterparty fields of the payment base are meaningful for an item or only
   the order details are.
6. **Budget sign:** whether an expense budget is stored as a positive or a
   negative `budget_amount`. The reader and writer pass the amount through
   unchanged; how the application compares budgets with totals was not
   established.
7. **Rule expressions:** how a rule is matched against a booking, including
   the marker characters `#`, `?` and `*`, and the order of rules and keywords
   in the canonical text under language settings other than German. (The
   character classes and the parsing are settled, 4.5.)
8. **`data_id`** of the import/export log entry: how it is derived from the
   file (a digest, a name).
9. **Localised names of the special categories.** Settled for the Banking4
   version described here: 4.3 lists them. Still open is whether other builds
   or rebranded editions use other texts (04, open question 3). A reader should
   rely on `special_category`, not on names.
10. **Loader rules are not confirmed.** In particular the constants in 3.4, the
    inversion rules in 3.5 (including the test for three upper-case letters)
    and the duplicate-ISIN failure in 3.6 are not confirmed. (Confirmed,
    on the well-formed demo vault only: the sort orders, and that
    existing special categories are not duplicated, see 8.4.) (The default
    title rule of 3.12.2 is confirmed on test files produced with Banking4,
    with German texts; the localised names of the document types in other
    languages are not known.)
11. **Document content.** That the content file holds the unmodified document
    bytes, and the extension table of 3.12.1, are not confirmed.
12. **Earlier and later layouts.** Only the category record carries a version
    number. For the other records, which trailing fields were added in which
    version is not known; older records simply end earlier and read as
    defaults. One real vault shows both directions (8.6): its category
    records are of version 8 and one byte shorter than the table, and its
    payee and portfolio item records are 2 bytes longer than the Banking4
    version described here writes. What a newer program version stores in those
    2 bytes is unknown; they were zero.
