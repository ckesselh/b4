# Notes on the database mapping

These notes explain the decisions behind the database that `b4 decrypt`
writes: how a value of the vault becomes a column value, what is left out,
and why. They are not part of the format description.

The tables, views and columns themselves, with types and descriptions, are in
[../schema.md](../schema.md). That file is generated from the schema
definition in the code (`src/db/schema.rs`), and `b4 schema` shows the same
texts from the database; these notes list no columns, so that nothing here
can drift from it. Field names of the vault are those of documents
[03](03-model-and-accounts.md), [04](04-payments-and-bookings.md) and
[05](05-master-data-and-portfolio.md).

The database is DuckDB. It was chosen for two column types: an exact
`DECIMAL`, so that sums over money are exact, and a real `DATE`.

## Principles

1. **One table per record type, one row per record.** No normalisation
   beyond what the vault has: the other party's name and account stay on the
   booking, a category is named by its text.
2. **Expose what Banking4 shows, keep what the file holds.** Where
   Banking4 changes a value after reading it (direction, scheme, currency,
   account kind, category name), the main column has the value
   Banking4 works with and a `stored_*` column has the stored one, but only
   where the two can differ.
3. **Every record the file holds is a row.** That includes records Banking4
   itself drops or hides when it loads a file (a payment in state `0x80`,
   mandates and exchange rates it filters out, a category without a
   usable name); the boolean column `hidden_by_banking4` marks them. A record
   is never left out because one of its values is odd. The one exception is
   the account record of kind 0, which is a placeholder and not an account
   (03).
4. **Nothing lossy without a trace, and no value in the trace.** A value that
   does not fit its column is rounded or stored as NULL, and a warning names
   the table, the column and the number of such values. A warning never
   contains a value from the vault, and there is no table of the odd values
   themselves: it would be a second, unstructured copy of vault data.
5. **No constraints the file does not keep.** No `UNIQUE`, no `PRIMARY KEY`
   and no `CHECK` on any table that holds records, and no ids made up by
   `b4`. The format guarantees no uniqueness, and a constraint would turn an
   unusual vault into a failed import.
6. **Bank access material is not exported.** The confidential part of a
   vault is what serves to log in at a bank or to authorise an order. Names,
   IBANs, amounts and purposes are exported in full; the access material is
   never read or dropped while decoding (see "What is left out").
7. **Stored values are not recomputed.** Derived arithmetic that can divide
   by zero or overflow (inverting an exchange rate, adjusting a price for a
   stock split, a market value) is not applied to any stored column. The
   split text of an instrument is stored as it is.
8. **Own names.** Tables and columns use this project's snake_case names.

## Identity and stamps

`id` is the object id of the record as stored, whatever it is: zero and
negative ids are kept. An id is unique within one collection file at most
(02 §5.1), so rows of per-account tables are identified by `account_id` and
`id` together, and price rows by the instrument and `id`. The account or
instrument comes from the name of the directory the file is in. A directory
that matches no account or instrument is decoded all the same, with a
warning, and its rows then refer to an account that has no row.

**Repeated ids.** A file can hold two records with the same id. Banking4
keeps both and gives the later one a fresh id that a reader cannot reproduce
(03, "Identity and time stamps"). `b4` keeps both rows with the id they have
in the file and reports the repetition in one warning. An invented id could
collide with a real one, and the stored id would be lost. The consequence
for queries: a join on the account and the id can match more than one row in
such a vault. None is expected in a healthy one.

**Stamps.** The object id and the update stamp are seconds since
2000-01-01T00:00:00 UTC (`unix_seconds = stamp + 946684800`) and become
`TIMESTAMP` columns in UTC. The creation time is derived from the id and is
approximate: ids run ahead of the clock when many records are created within
one second. The change time is NULL for an update stamp of -1 ("not
stamped") and of 0: Banking4 stores 0 for any time before 2000, so 0 does
not say when the record was changed (02 §5.1). A negative stamp other than
-1 is a time before 2000 and is converted like any other.

## Dates, times and time zones

**Calendar dates** become `DATE`, and "no date" is NULL. For the two reader
rules that clean a date (`date_clamped`, `date_strict`) the rule of 02 §4.12
is applied first. A `date_raw` is stored when year, month and day form a real
calendar date; 9999-12-31 stays what it is (Banking4 uses it as "end of
time") and 0001-01-01 is "no date", as in Banking4. Anything else (a year
alone, month 13, 31 February, a negative year) is NULL in the column and is
counted in a warning. It is not clamped silently: Banking4's own conversion
would turn a year-only value into 1 January, which looks like data. The row
is kept in every case.

**Times stored as ticks** become `TIMESTAMP` without a zone; 0 is NULL. The
format does not say whether such a value is UTC or local wall-clock time, and
that is open per field ([README](README.md), question 4); the column
descriptions say "zone unknown". Many of these fields hold a day at midnight
(the validity of a tax exemption, a settlement day). They stay timestamps:
turning them into dates would hide a possible shift by one day. The column
has microsecond precision, so the seventh fractional digit of a tick value
is cut. Where the account record stores a moment twice, as a day and as
ticks, the ticks win when set and the day at midnight is used otherwise.

**Times of day** become `TIME` with one-second resolution; "no time" is
NULL, midnight is `00:00:00`.

Only the stamps of the previous section and the entries of the `meta` table
are known to be UTC.

## Decimals, rounding and warnings

A decimal of the vault is a 96-bit integer with a scale of 0 to 28 and a
sign: up to 29 significant digits. No single `DECIMAL(p, s)` holds every
such value, so two column types are used:

- **Money** (amounts, balances, limits): `DECIMAL(18,4)`. Sums and
  comparisons over it are exact.
- **Quantities, prices and rates** (and the unsigned stored amount of the
  payment family): `DECIMAL(28,10)`. They are multiplied rather than summed
  and need more decimals.

Every value is checked against its column before it is inserted. A value
with more decimals than the column has is rounded to the column's scale,
half to even. A value with more digits before the point than the column
holds is stored as NULL. Either case is counted per table and column and
reported as one warning with the count, never with a value. The demo vault
needs neither.

The scale of a stored value is not visible in a `DECIMAL` column (`12.30`
and `12.3` are the same number), and the plain `0` that Banking4 puts in
place of a value for some records (03, 05) is just zero.

Warnings are printed by `b4 decrypt` and stored in the `warnings` table,
which `b4 status` shows. They cover what the reader reported while loading
([README](README.md), "Coping with other versions") and the values that did
not fit. They name files, tables, columns, record numbers and counts.

## Signed amounts and the stored direction

The payment family (bookings, payments, standing orders) stores an amount
without sign and a direction that is partly a bit and partly derived from
the scheme (04 §5.5). In the database:

- `amount` is **signed**: negative when money leaves the account, positive
  when it arrives. It is the stored amount, negated when the effective
  direction is debit, which is Banking4's own rule.
- `debit` is the *effective* direction: the stored bit for bookings and for
  payments without a scheme, the scheme's direction otherwise.
- The stored amount, and for payments and standing orders the stored
  direction bit and the stored scheme, are kept next to them, because there
  they can differ from the effective values. Never negate by the stored bit
  alone.

An asset stores a signed amount already; it is kept, with its direction.
Other amounts have no direction and are stored as they are: the VAT part of
an amount, a payee's default amount, tax exemption amounts, the settlement
amount of a portfolio transaction (its delivery direction says which way the
securities moved). The sign convention of a booking's instructed amount and
charges, and of a category's budget, is open ([README](README.md),
question 19); they are stored as delivered.

The currency column next to `amount` is the one Banking4 shows (for a
booking the currency of its account, 04 §6.2); the stored code is kept
beside it.

## `counted`: splits and batches

A booking that is the master of a split or of a batch is a row of its own,
and so is each of its parts; their amounts need not agree (for a batch they
usually do not, 04 §6.4). Money is counted once by counting the parts and
the ordinary bookings and leaving out every master that has parts. Counting
masters instead is not equivalent.

The column `counted` says which rows count. `sum(amount)` over the counted
rows of an account is its balance movement; a sum over all rows counts money
twice. The roles used for this are those after the regrouping of 04 §6.4,
not the stored byte. `bookings_view` filters nothing out, so a query on it
needs the condition too; `b4 bookings summary`, the counts of the list
commands and the views that sum apply it.

Pending bookings are a second, independent condition: they are rows with a
signed amount like any other, but not part of the account's stored balance.

## Texts and category keys

- An empty text and a missing text are the same thing in the format: NULL.
- **Texts are stored as decoded.** Nothing is trimmed and no line break is
  changed, except where a reader rule of the format says so (category names,
  the parts of an address, the name and the additional text of a portfolio
  item). A payee's notes with CR LF and an empty line come out with both.
- **The purpose text of the payment family** is one column with the lines
  joined by a line feed, after the reader's rule of 04 §5.6 (cut at CR, LF
  or CR LF; drop empty lines; do not trim). For a legacy single-chunk record
  it is the text left after the tagged lines were taken out (04 §5.8).
- **Category names** are stored normalised (04 §5.7), with the name as
  stored kept in the catalogue. Records name their category by text; there
  are no category ids to join on. For joins there is `category_key`: the
  name trimmed, with every character upper-cased on its own. The
  upper-casing is the simple one-to-one mapping and never changes the number
  of characters: a character whose upper-case form would be more than one
  character stays as it is. So `Straße` and `Strasse` have different keys,
  as they are different categories in Banking4 (05 §4.1). The key is what
  makes names that differ only in case one category. It is **not unique**:
  two category records can have the same key, and a booking can use a key
  that no category record has. The complete tree is the union of the keys of
  the catalogue and the keys the bookings use (05 §4.6).
- The `rules` of a category are the stored expression, unparsed. Its layout
  version is stored next to it, because the same text means different things
  below version 4 and from it on (05 §4.5).

## Coded values

- **Enumerations** are `VARCHAR` with this project's lower-case snake_case
  name of the member. A value `b4` does not know is its decimal number as
  text. There is no `ENUM` type: a value from a newer Banking4 must not make
  the import fail.
- **Flag sets** are `VARCHAR` with the names of the set flags, joined by
  commas in the order of their declaration, NULL when none is set. A flag
  people filter on has a `BOOLEAN` column of its own as well (an account's
  `hidden`, for example).
- A reference to another record is NULL where the vault stores "none" (0,
  and -1 where the format uses it).
- A text that Banking4 would parse and can fail on (the payee
  verification result of a payment, the splits of an instrument) is stored
  as the raw text.

## Derived values that are and are not there

Stored for convenience, and derived from stored values only: `counted`,
`pending`, the creation time, the category key and the levels of a category
name, the canonical ISIN next to the stored identifier text (05 §5.1; NULL
when the text yields none), and the views.

Deliberately absent: the next execution day of a standing order held at the
bank (Banking4 discards the stored date and computes one; the computation is
not specified, so the column is NULL and the stored date is kept beside it;
[README](README.md), question 14), the market value of a position, split
adjusted prices, inverted exchange rates, the transaction code Banking4
displays instead of the stored one, the link between the two sides of an
internal transfer, and the running balance after each booking (04 §6.7).
What the vault holds for a balance is exported: the account's statement
balance with its time, and every booking with its signed amount, status and
`counted`; the view `account_balances` puts the two side by side.

## What is left out

### Bank access material

Never in the database, whatever else is decided. There are two mechanisms
(03, "Entries").

**Entries that are never read.** Nine entries of the root directory are
skipped by name before a byte of their content is read: `Contacts`,
`EbicsContacts`, `ContactsHTML`, `SpoolerContacts`, `OnlineContactKeys`,
`Outbox`, `OutboxData`, `ModelSettings` and `OnlineTraces`. The first five
are the stored bank accesses; the outbox holds user ids, TANs and bank
contacts of orders; the settings hold a crypto key and API tokens among
harmless preferences, and no allow-list of harmless keys is attempted; the
traces are bank protocol logs. Only their number is recorded.

**Fields that are dropped at decode time.** The decoder reads them to
advance and discards them, so they never exist in a memory structure, in
any output or in the database:

| Record | Fields | What they are |
|---|---|---|
| account | `fints_customer_id` | Login identifier for online banking |
| account | `last_download_token` | Continuation token of the last download |
| account | `contact_guid`, `order_contact_guid` | Identify the stored bank access |
| account | `contact_name`, `order_contact_name` | Name the stored bank access |
| account | `fints_account_sub_id` | Sub-account discriminator of the online banking dialog |
| payment | `used_tan`, `order_reference_tan` | One-time authorisation values |
| account document (not read in this version) | `acknowledge_token` | Token presented to the bank to acknowledge a document |

The `accounts` and `payments` tables have an explicit column list, and a
test asserts that none of these names is a column.

A reader also never touches the host file system on behalf of a name stored
in a vault (the file name of a trace or of a document).

### Record types without a table

| Record | Why |
|---|---|
| setting, outbox order, online trace | Never read, see above |
| booking filter | User interface state |
| signature folder item and split | Online banking workflow; no test files in the repository for the layout |
| invoice | Specified, not exported in this version |
| account document and document contents | Not exported in this version. Not harmless: each record contains `acknowledge_token` |
| import/export log, payee pictures | Logs and pictures |

Their entries are skipped without being read. An entry whose name `b4` does
not know at all is skipped unread too, with a warning that names it.

### Fields without a column

Fields the format marks as unused or reserved; the housekeeping and
notification preferences of an account (retention periods, notification
limits, what a synchronisation run fetches, the position of a third-party
bookkeeping service) and its list of IBANs for internal transfers; the
payment-side fields of the shared payment base on a booking (scheme,
schedule, cost bearer account), which are always empty there. The
day-precision dates of the account are merged into the timestamp columns
(see "Dates, times and time zones").

Values that exist only in Banking4's memory (running balance, day balance,
cross entry link, the expansion state of a batch, name digests) are not in
the vault and therefore not in the database.
