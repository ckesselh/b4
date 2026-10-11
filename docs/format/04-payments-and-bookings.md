# 04 Payments and bookings

Record layouts for the payment family: the shared payment base, bookings
(statement entries), payments, standing orders, the split editing row and the
outbox order.

Everything here describes how one version of Banking4 reads and writes these
records, for the purpose of reading a user's own vault. The text calls it "the
Banking4 version described here"; it is the build named in 02 section 0. Field
names are ours. Section 13 says which statements are covered by the fixtures
in `tests/fixtures/records/`, which are confirmed against data exported from
Banking4, and which are not confirmed. Section 14 lists what is still open.

## 1. Where the records live

All of these are collection files: a sequence of `[i32 size][record]`, one
record per object, no header and no trailer.

| Vault path | Record | Notes |
|---|---|---|
| `Acct<oid>/Stmt` | booking | One file per account. `<oid>` is the account's object id in decimal. |
| `Acct<oid>/StmtMasterEntries` | booking | Legacy. If present, Banking4 loads it after `Stmt` with the same record reader (the layout of section 6, including the account-dependent adjustments of 6.2), merges its records into the booking list and deletes the file. A reader should treat it as more bookings of the same account and mention the file in a warning. |
| `Acct<oid>/Paymts` | payment | Payments that were entered, queued or sent, and their batches. |
| `Acct<oid>/Stos` | standing order | |
| `Outbox` | outbox order | Top level, one file for the whole vault. |
| `OutboxData/<oid>.xml` | (XML document) | Order payload of an outbox order that has not been completed yet. Not a record file. |

The split editing row (section 7) has a record layout but no file: Banking4
starts with an empty list of such rows while the split dialog is open and
never loads that list from or saves it to the vault. A finished split is
stored as ordinary booking records (section 6.4).

There is a fourth member of the payment family (signature folder items,
`Acct<oid>/AcctPayChecks`, called pay checks in document 03). It shares the
payment base described here; its own fields are specified in document 05,
section 3.13, and the base reader treats it like a booking (5.4).

The complete file map of a vault, with the account kinds for which each of
these files exists, is in document 03, Part 1.

## 2. Wire primitives

The primitives are defined in document 02 (record wire format). They are
summarised here, under the same names, so that this file can be read on its
own. All integers are little-endian.

| Primitive | Bytes | Encoding | Value when the chunk has too few bytes left |
|---|---|---|---|
| `u8` | 1 | Unsigned. | 0 |
| `bool` | 1 | 0 is false, anything else is true. Written as 0 or 1. | false |
| `u16` | 2 | Unsigned, 0..65535. | 0 |
| `i32` | 4 | Signed. | 0 |
| `decimal` | 16 | Four `i32` words: low, middle and high word of an unsigned 96-bit mantissa, then a flag word with the scale (0..28) in bits 16..23 and the sign in bit 31. Value = mantissa / 10^scale, negated if the sign bit is set. The scale is preserved, so `12.30` and `12.3` are different byte strings. | 0 |
| `string` | 2+n or 6+n | `u16` length n in bytes. If that length is `0xFFFF`, an `i32` length follows and is used instead. Then n bytes of UTF-8 without a byte order mark. Length 0 means "no value": there is no distinction between a missing and an empty string, the writer emits length 0 for both and the reader returns "no value". Invalid UTF-8 is decoded leniently (replacement characters), not rejected. | no value |
| `code(n)` | n | Exactly n bytes, one character per byte (byte value = code point). After reading, trailing NUL bytes are removed and the rest is trimmed of white space; an empty result is "no value". The writer pads with NUL and truncates to n. | no value |
| `date_raw` | 4 | `u16` year, `u8` month, `u8` day. Four zero bytes mean "no date". The reader does not validate the three parts: a year without month and day, month 13 or 31 February come back as stored. A year of `0x8000` or above comes back negative and the read goes on normally (document 02, section 4.12). `b4` keeps the parts as read; in the database a value that is not a real calendar date is stored as NULL with a warning, and the record is kept. | no date |
| `date_clamped` | 4 | Same bytes as `date_raw`, but read with validation: if year, month or day is 0 the result is "no date", and so is 0001-01-01 (`01 00 01 01`); the year is unsigned here, and a year above 9999 becomes 9999 (so `FF FF 01 01` is 9999-01-01, where `date_raw` sees the year -1); a month above 12 becomes 12, and a day above 28 is reduced to the last day of that month if it exceeds it. 9999-12-31 is returned as that plain date. | no date |
| `time_hms` | 3 | Hour, minute, second, one byte each. Hour `0xFF` means "no time"; the writer emits `FF FF FF`. | no time |
| `ticks` | 8 | `i64`, count of 100 ns units since 0001-01-01 00:00:00 with no time zone attached. 0 means "no timestamp". | no timestamp |
| `guid` | 16 | The first three groups of the textual form are stored little-endian, the last two as written: `00112233-4455-6677-8899-aabbccddeeff` is `33 22 11 00 55 44 77 66 88 99 aa bb cc dd ee ff`. | all zero |

Rules that apply to every primitive:

- A fixed-size value is read whole or not at all. If fewer bytes than its size
  remain in the current chunk, the default from the last column is returned and
  the read position does not move.
- A `string` whose length runs past the end of the chunk yields "no value" and
  moves the read position to the end of the whole record, so every later read
  in that chunk yields its default too. The read position can then lie beyond
  the end of the chunk; the number of bytes left must be computed in signed
  arithmetic or clamped at zero (document 02, section 3).
- A count that is followed by that many elements (only the outbox order has
  such lists here, 10.1) is never used to allocate. It is refused when it is
  larger than the bytes left in the chunk divided by the smallest size of one
  element, and that comparison is made as a division or in checked
  arithmetic, never as a 32-bit product (document 02, section 8, rule 11).
- A `string` is returned as stored. Nothing is trimmed on read.
- The long form of the `string` length is accepted for any length, not only
  for lengths of `0xFFFF` and above.
- Banking4 stops loading the file at a record that contains a value the
  runtime it runs on rejects, and reports the file as damaged; records before
  it stay loaded. Such values are: a `decimal` flag word with a scale above 28
  or with other bits set, a `time_hms` with hour above 23 (other than `0xFF`)
  or minute or second above 59, a `ticks` value below 0 or beyond the end of
  year 9999. A tolerant reader has to pick its own behaviour for those; they
  cannot occur in a file that Banking4 loads completely.

## 3. Record and chunk structure

A record is one or more chunks. Each chunk starts with an `i32` size word:
bits 0..29 are the chunk size in bytes including the size word itself, bit 31
is set when another chunk follows, bit 30 is not used.

Two operations matter for the layouts below:

- "more chunks follow" is bit 31 of the size word of the chunk currently being
  read.
- "go to next chunk" moves the read position to the start of the next chunk and
  opens it. If the current chunk is the last one, it does nothing at all: the
  read position stays where it is, inside the current chunk.

The payment family uses this as follows.

| | Chunk 1 | Chunk 2 |
|---|---|---|
| Current records | object header, payment base head, payment base extension | fields of the concrete record type |
| Legacy single-chunk records | object header, payment base head, then directly the fields of the concrete record type | (does not exist) |

So a booking, a payment or a standing order written by a current version always
has exactly two chunks. The writer never produces the single-chunk form; the
reader still accepts it (section 5.2).

After a record has been decoded and added, Banking4 checks that no chunk is
left unread. If one is, it stops loading the file there and reports it as
written by a newer version. A reader that wants to survive future versions
should skip surplus chunks instead.

Bytes of a chunk that follow the last field are skipped, and so are bytes of
the record that follow its last chunk. A set continuation bit with no valid
chunk behind it makes Banking4 reject the record (document 02, section 2).

Within a chunk, a record may end before the last field of the tables below.
The short-chunk defaults of section 2 then apply to the missing fields. That
appears to be how the format takes records written before a field was added,
so a reader must implement those defaults rather than treat a short chunk as
damage.

The outbox order and the split editing row are single-chunk records and never
call "go to next chunk".

## 4. Object header

Every record starts with the same two values, read before anything else. They
are the record base of document 03; the stamp encoding is defined in document
02.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object id, unique within its collection file. | Other records refer to it (batch ids, reconciled booking, outbox link). If Banking4 meets an id twice in one file it keeps both records and gives the later one a fresh id, which a reader cannot reproduce; report duplicates instead. |
| 2 | 1 | `i32` | `update_stamp` | Time of the last change, in seconds since 2000-01-01 00:00:00 UTC. | -1 means "not stamped yet"; the writer replaces -1 by the current time when it saves the record. Store raw. |

New object ids are drawn from the same clock as the stamp (seconds since
2000-01-01 UTC, increased by one if that value is not above the highest id
seen so far), so an `oid` usually also tells when the object was created.
Readers should still treat it as an opaque key.

## 5. Payment base

The base of bookings, payments and standing orders. It continues chunk 1 right
after the object header.

### 5.1 Head

Always present.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `u8` | `class_and_direction` | Bits 4..7: `scheme_class` (section 11.1). Bit 0: `debit_flag`. | Bits 1..3 are written as 0 and ignored on read. See 5.4 and 5.5 for what the two parts mean after loading. |
| 2 | 1 | `decimal` | `amount` | Amount without sign. | The application never stores a negative amount, but the reader does not check. |
| 3 | 1 | `code(3)` | `currency` | ISO 4217 code. | Overridden for bookings, see 6.2. |
| 4 | 1 | `string` | `counterparty_name` | Name of the other party (payee or payer). | |
| 5 | 1 | `string` | `counterparty_account` | IBAN, or a national account number. | |
| 6 | 1 | `string` | `counterparty_bank_code` | BIC, or a national bank code. | |
| 7 | 1 | `code(2)` | `counterparty_country` | ISO 3166 country code. | |
| 8 | 1 | `string` | `purpose_text` | Remittance information as one text. | Not kept as text: turned into `purpose_lines`, see 5.6. |
| 9 | 1 | `date_raw` | `scheduled_date` | Requested execution date. | Sort key of payments (6.4). |
| 10 | 1 | `string` | `order_id` | Reference the bank assigned to a submitted order. | A booking receives it from the payment it was reconciled with. |
| 11 | 1 | `string` | `category` | Full category name assigned by the user, levels separated by `:`. | Normalised on read, see 5.7. |

### 5.2 The "more chunks follow" test

Directly after `category` the reader tests whether more chunks follow the
current one. It does not move the read position.

- **More chunks follow** (every record a current version writes): the
  purpose lines are taken as they are, and the extension fields of 5.3 are read
  from the rest of chunk 1.
- **No more chunks** (legacy single-chunk record): nothing further is read for
  the base. The extension fields do not exist on the wire. Instead, the purpose
  lines are scanned for tagged lines, which fill some of the extension fields
  (section 5.8). The concrete record type then calls "go to next chunk", which
  does nothing in a single-chunk record, and reads its own fields from the same
  chunk, starting at the byte after `category`.

This reads as the upgrade path from an older storage format in which the base
had only the eleven head fields and everything else was folded into the
purpose text. The history of that format is not known. What the Banking4
version described here does with such a record is specified here.

### 5.3 Extension

Read only when more chunks follow.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 12 | 1 | `string` | `counterparty_street` | Street and building number. | |
| 13 | 1 | `string` | `counterparty_town` | Post code and town. | |
| 14 | 1 | `string` | `end_to_end_id` | SEPA end-to-end reference. | |
| 15 | 1 | `string` | `payment_info_id` | SEPA payment information id (batch reference). | |
| 16 | 1 | `string` | `purpose_code` | SEPA purpose code, four letters. | |
| 17 | 1 | `string` | `category_purpose_code` | SEPA category purpose code. | |
| 18 | 1 | `string` | `mandate_id` | SEPA direct debit mandate reference. | |
| 19 | 1 | `date_raw` | `mandate_signed_date` | Date the mandate was signed. | |
| 20 | 1 | `string` | `creditor_id` | SEPA creditor identifier. | |
| 21 | 1 | `string` | `local_instrument_code` | SEPA local instrument (`CORE`, `B2B`, `INST`, ...). | Input to the scheme derivation in 5.4. |
| 22 | 1 | `string` | `sequence_type` | SEPA sequence type (`FRST`, `RCUR`, `OOFF`, `FNAL`). | |
| 23 | 1 | `string` | `ultimate_counterparty_name` | Ultimate creditor or debtor, if different from the counterparty. | |
| 24 | 1 | `u8` | `cost_bearer` | Who pays the charges of a foreign payment (section 11.3). | |
| 25 | 1 | `i32` | `cost_bearer_account_oid` | Object id of the account charged for the fees. | 0 if none. |
| 26 | 1 | `string` | `intl_remittance_method` | Payment method code of a foreign payment. | |
| 27 | 1 | `string` | `intl_remittance_instruction` | Free instruction text of a foreign payment. | |
| 28 | 1 | `string` | `counterparty_bank_name` | Name of the counterparty's bank. | |
| 29 | 1 | `string` | `counterparty_bank_street` | Street of that bank. | |
| 30 | 1 | `string` | `counterparty_bank_town` | Town of that bank. | |
| 31 | 1 | `i32` | `scheme_type` | Payment scheme (section 11.2). | Adjusted after reading, see 5.4. |
| 32 | 1 | `time_hms` | `scheduled_time` | Requested execution time. | Banking4 reports it only when `scheme_type` is 8 (instant credit transfer) and "no time" for every other scheme, whatever is stored. |
| 33 | 1 | `string` | `regulatory_reporting_code` | Regulatory reporting purpose code of a foreign payment. | |

### 5.4 Scheme type after reading

The last step of the base reader depends on the kind of record:

- **Booking** (and the signature folder item): `scheme_type` is set to 0
  regardless of what was read.
- **Payment and standing order**: if `scheme_type` is 0 (which is always the
  case for a legacy record, where it was not read), it is derived from the
  `scheme_class` nibble, the `debit_flag` bit and `local_instrument_code`:

| `scheme_class` | `debit_flag` set | `debit_flag` clear |
|---|---|---|
| 0 any | 1 user expense | 2 user income |
| 1 sepa | 8 instant credit transfer if `local_instrument_code` is exactly `INST`, otherwise 5 credit transfer | 7 direct debit B2B if `local_instrument_code` is exactly `B2B`, otherwise 6 direct debit core |
| 2 foreign (legacy file format) | 4 | stays 0 |
| 3 domestic (legacy file format) | 3 | stays 0 |
| anything else | stays 0 | stays 0 |

A reader must replicate this to show the same scheme as Banking4. The nibble
itself is not kept by the application; it is recomputed from `scheme_type` when
writing (5.9).

### 5.5 Amount and direction

`amount` is stored without sign. The direction is a separate piece of
information, and where it comes from depends on the record:

- If `scheme_type` (after 5.4) is 0, the direction is the `debit_flag` bit.
- If `scheme_type` is not 0, the bit is ignored and the direction follows from
  the scheme: user income (2) and the two direct debit schemes (6, 7) are
  credits, every other scheme is a debit.

"Debit" means money leaving the account. The signed value Banking4 works with is
`-amount` for a debit and `+amount` otherwise.

Consequences per record type:

| Record | Where the sign lives |
|---|---|
| Booking | Always the `debit_flag` bit, because `scheme_type` is forced to 0. |
| Payment, standing order | The scheme. The bit only matters while deriving a missing scheme (5.4) or when the derivation leaves the scheme at 0. In records written by a current version the bit agrees with the scheme, because the writer stores the effective direction, not the raw bit. |
| Split editing row | Its own `bool`, see section 7. |
| Outbox order | No direction at all. The amount is the order amount and the signed value equals it. |

A reader should store `amount`, the raw bit and `scheme_type`, and compute the
effective direction with the rule above.

### 5.6 Purpose text and purpose lines

On the wire the purpose is one `string`. In memory it is a list of lines:

1. "No value" gives an empty list.
2. The text is cut at every line break. A line break is a single CR, a single
   LF, or CR immediately followed by LF. No other character separates lines.
3. Pieces of length 0 are dropped. That removes blank lines and the empty piece
   after a trailing line break. Lines are not trimmed and not wrapped: leading
   and trailing spaces survive, and a line can be arbitrarily long.

The writer joins the lines with a single LF and writes "no value" for an empty
list. So the mapping is not reversible for texts that contained CR, blank lines
or a trailing line break, but it is stable: reading what the writer wrote gives
the same lines again.

A reader must replicate steps 1 to 3 to obtain the same lines. It may equally
well store the raw text and split on demand, as long as blank lines are
dropped.

In a current (two-chunk) record the lines are stored exactly as produced by
these steps. Tag prefixes such as `EREF+` that appear in such a record are part
of the text the bank delivered and are not interpreted on load.

### 5.7 Category normalisation

The stored category string is passed through these steps, in this order:

1. "No value" stays "no value".
2. Trim white space at both ends. "Trim" here and in step 5 is the string
   trimming of the runtime Banking4 runs on: it removes every character of
   the Unicode `White_Space` set (for the first 256 code points `09`..`0D`,
   `20`, `85` and `A0`, as in document 02, section 4.9; beyond them the
   Unicode space, line and paragraph separators). `1C`..`1F` are not
   trimmed. Rust's `str::trim` removes the same set; the one character
   runtimes disagree about is U+180E, which older Unicode versions classed as
   a space. The test files of document 02 cover the first 256 code points
   only; beyond them the set is not confirmed.
3. If the result is empty (the stored string consisted of white space only), it
   becomes the localised full name of the built-in "do not evaluate" category.
4. If the result equals the localised name of one of three pseudo categories
   ("automatic", "not set", "catalogue root"), it becomes "no value".
5. Otherwise split at every `:`, trim each part, replace a part that is empty
   after trimming by the literal `UNBEKANNT`, and join the parts with `:` again.

The localised names are user interface texts and depend on the language
Banking4 runs in:

| | German (default, also used for Swiss German) | English | French |
|---|---|---|---|
| catalogue root | `(Kategorienkatalog)` | `(Catalog of Categories)` | `(Catalogue des catégories)` |
| automatic | `(automatisch)` | `(automatic)` | `(automatique)` |
| not set | `(nicht gesetzt)` | `(not set)` | `(non activé)` |
| do not evaluate | `(nicht auswerten)` | `(exclude)` | `(ne pas évaluer)` |

A language Banking4 has no texts for uses the German ones. The comparison in
step 4 is exact and case-sensitive.

A reader has to replicate steps 2 and 5 to get category names that match the
category list. Steps 3 and 4 only matter for odd stored values; a reader that
does not know the language of the installation that wrote the vault should
treat the names of all three languages as pseudo categories in step 4, and has
to pick one language for step 3 (German is the product's default).

The writer does **not** normalise. It stores the category exactly as it is
held in memory (5.9). What is in memory is normalised all the same in practice:
a record that was read went through the steps above, and a category the user
assigns in the application is looked up in the category catalogue and replaced
by the full name of the catalogue entry before it is stored (not confirmed,
13.5). So for records written by a current version the steps are normally a
no-op, but a reader must not rely on that.

For a batch or split master (section 6.4) Banking4 displays no category even if
one is stored.

### 5.8 Legacy records: tagged purpose lines

Applies only when no more chunks follow (5.2). The purpose lines obtained in 5.6
are processed one by one, in order, and a new list of purpose lines is built.

A line is *tagged* if it is at least five characters long and its fifth
character is `+`. The tag is the first four characters, the value is everything
after the `+` (possibly empty). Untagged lines are appended to the new list
unchanged.

| Tag | Effect |
|---|---|
| `EREF` | `end_to_end_id` = value, trimmed; "no value" if empty, `NONREF` or `NOTPROVIDED`. |
| `KREF` | `payment_info_id`, same treatment as `EREF`. |
| `MREF` | `mandate_id` = value, verbatim. |
| `CRED`, `DEBT` | `creditor_id` = value, verbatim. |
| `SVWZ` | Append the value to the new list (dropped if empty). |
| `TXTS` | Text key, see below. |
| `MDOS`, `MDAT` | `mandate_signed_date` = value parsed as eight digits `YYYYMMDD`. `00000000` and an empty value are "no date". Any other value that is not exactly eight digits, or has a month of 0 or above 12, a day of 0 or above 31, or year 0, is ignored and leaves the field as it was. |
| `PADR` | Split the value at every U+0001 character and drop empty parts. First remaining part is `counterparty_street`, second is `counterparty_town`; a missing part gives "no value". |
| `BNAM` | `counterparty_bank_name` = value. |
| `BADR` | As `PADR`, for `counterparty_bank_street` and `counterparty_bank_town`. |
| `PTCB` | `cost_bearer`: `OUR` is 1, `SHA` is 2, `BEN` is 3, anything else 0. |
| `PTCA` | `cost_bearer_account_oid` = value as a decimal integer, 0 if it does not parse. |
| `PTMT` | `intl_remittance_method` = value. |
| `PT20` | `intl_remittance_instruction` = value. |
| `PURP` | If the value is longer than four characters: `purpose_code` = first four characters, and the remainder is appended to the new list. Otherwise, if the value is `CORT` or `INTC`: `category_purpose_code` = value. Otherwise `purpose_code` = value. |
| `CPCD` | `category_purpose_code` = value. |
| `LCIC` | `local_instrument_code` = value. |
| `SQTP` | `sequence_type` = value. |
| `ABWA` | Remember the value as *deviating originator*. |
| `ABWE` | Remember the value as *deviating recipient*. |
| any other | Append the whole line, tag included, to the new list. |

Tags that assign a field overwrite it each time they occur, so the last
occurrence wins. Tags that append a line append one per occurrence.

`TXTS`: ignored if the value has fewer than two characters. Otherwise `key` is
the first two characters and `ext` is characters three to five if the value has
at least five characters (else absent). What happens depends on the
`scheme_class` nibble:

- class 1 (sepa) with `debit_flag` set: `purpose_code` is derived from the
  numeric values of `key` and `ext` (0 if absent or not a number):

  | `key` | `purpose_code` |
  |---|---|
  | 6, 106, 182, 190 | by `ext`: 3 or 23 is `CDCD`, 73 is `MTUP`, 24 is `ETUP`, everything else `DCRD` |
  | 107 | `CGDD` |
  | 8, 52, 152 | `RINP` |
  | 53, 80, 153 | `SALA` |
  | 54, 154 | `CBFF` |
  | 155 | `CBFR` |
  | 56, 156 | `GOVT` |
  | 17, 67, 167 | `IVPT` |
  | 19, 69, 119, 169 | `CHAR` |
  | 82, 83 | `CDCD` |
  | 801..813 | `NCHG` |
  | anything else | "no value" |

  Since `key` has two characters, only the rows with values below 100 can
  actually match.
- class 1 (sepa) with `debit_flag` clear: `local_instrument_code` is `B2B` for
  `key` `04`, `74`, `84`, `08`, `96`, `97`; `CORE` for `05`, `95`, `09`, `71`,
  `81`, `92`; otherwise "no value". `sequence_type` is `FRST`, `RCUR`, `OOFF`,
  `FNAL` for `ext` `991`, `992`, `993`, `994`; otherwise "no value". Both
  fields are assigned even when the result is "no value".
- class 2 (foreign): `intl_remittance_method` = `key`.
- any other class: nothing.

After the last line, if neither `ABWA` nor `ABWE` was seen, nothing more
happens. Otherwise:

- If `creditor_id` has a value and `debit_flag` is set, or `creditor_id` has no
  value and `debit_flag` is clear: `ultimate_counterparty_name` = the deviating
  originator (which may be absent), and if a deviating recipient was seen, the
  line `ABWE+` followed by it is appended to the new list.
- In the other two cases: `ultimate_counterparty_name` = the deviating
  recipient, and if a deviating originator was seen, the line `ABWA+` followed
  by it is appended.

The new list replaces the purpose lines. `scheme_type`, `scheduled_time` and
`regulatory_reporting_code` stay empty, and 5.4 then derives the scheme for
payments and standing orders, using a `local_instrument_code` that `TXTS` or
`LCIC` may have set.

A reader that wants to support single-chunk records has to replicate this
section completely; none of the affected fields exist on the wire in that
case. A reader that only supports current records can skip it, but must then
reject single-chunk records of these types instead of guessing.

### 5.9 Writer

The writer emits the object header, the eleven head fields and the 22
extension fields in exactly the order of 5.1 and 5.3, always, in one chunk.
Differences between what is written and what was read:

| Field | Written value |
|---|---|
| `class_and_direction` | High nibble: the class that belongs to the current `scheme_type` (section 11.2), not the nibble that was read. Bit 0: the effective direction of 5.5, not the raw bit. |
| `purpose_text` | Purpose lines joined with LF (5.6). |
| `category` | The in-memory name as it is, without normalising it again (5.7). |
| `scheme_type` | The in-memory value, that is the value after 5.4, so a derived scheme becomes explicit, and a booking that was read always stores 0. |

Everything else is written from the in-memory field without any check: a
negative amount, a scheme type no table row stands for, an unvalidated date.
A `code` field longer than its width is cut, an empty string is written like
"no value".

Banking4 never writes the single-chunk form. A legacy single-chunk
record that is read and saved again comes out as a two-chunk record, with the
fields that 5.8 extracted from the purpose text now in the extension and the
remaining purpose lines in `purpose_text`.

### 5.10 Batch fields

Three more values belong to the base in memory but are not part of it on the
wire:

| Own field name | Source |
|---|---|
| `batch_oid` | Read by the concrete record type (booking: 6.1 position 9, payment: 8.1 position 5). Standing orders have none; the in-memory value stays at its initial -1. |
| `batch_index` | Never stored. Recomputed after loading, see 6.4. |
| `batch_expanded` | Never stored. User interface state. |

`batch_oid` values 0 and -1 both mean "not in a batch". A record whose
`batch_oid` equals its own `oid` is a batch master; any other value makes it a
slave of the master with that `oid`.

One more in-memory value of the base is never stored: a cached digest of the
counterparty name used for matching. It is absent after reading.

### 5.11 Displayed currency

The stored `currency` is not always what Banking4 reports as the currency of a
record. For a booking it always reports the currency of the account (6.2). For
a payment and a standing order:

- if `currency` has a value, that value;
- otherwise it depends on `scheme_type` (after 5.4):

| `scheme_type` | Reported currency when none is stored |
|---|---|
| 1 user expense, 2 user income | the currency of the account (with the fallback of 6.2 step 3 if the account has none) |
| 3 domestic legacy credit transfer | `DEM` |
| 5, 6, 7, 8 (SEPA credit transfer, both direct debits, instant credit transfer) | `EUR` |
| 0, 4, 9, 10, 11 and anything else | none |

The writer stores the field, not the reported value, so a payment without a
stored currency stays without one.

A reader that wants to show the same currency as Banking4 has to replicate
this; it needs the account record for the first row.

## 6. Booking

File `Acct<oid>/Stmt`. Object header, payment base, then:

### 6.1 Read order

"Go to next chunk" is called first. In a current record this opens chunk 2; in
a legacy single-chunk record it does nothing and the fields below follow the
`category` string in chunk 1.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 2 | `date_raw` | `booking_date` | Date the bank booked the entry. | Empty for entries that are not booked yet. Sort key. |
| 2 | 2 | `date_raw` | `value_date` | Value date. | Cleared for cash accounts, see 6.2. |
| 3 | 2 | `i32` | `transaction_code` | Kind of transaction as stored (section 11.7). | The value Banking4 displays can differ, see 6.6. |
| 4 | 2 | `string` | `booking_text` | Booking text supplied by the bank ("Buchungstext"). | |
| 5 | 2 | `string` | `primanota` | Primanota number. | |
| 6 | 2 | `u8` | `entry_bits` | Bit set (section 11.5). | Bit 0 is the read/unread marker. |
| 7 | 2 | `i32` | `flag` | Colour flag (section 11.6). | Values outside 0..7 become 0, see 6.2. |
| 8 | 2 | `u8` | `split` | Role in a split or batch (section 11.8). | One byte, so only bits 0..7 can be stored. |
| 9 | 2 | `i32` | `batch_oid` | `oid` of the master this entry belongs to. | Not stored as read: adjusted in 6.2 and then placed in the base (5.10). |
| 10 | 2 | `string` | `bank_reference` | Reference assigned by the bank. | |
| 11 | 2 | `string` | `notes` | Free text note by the user. | |
| 12 | 2 | `u8` | `booking_status` | 0 booked, 1 pending, 2 information (section 11.4). | Adjusted in 6.2. |
| 13 | 2 | `u16` | `gvc` | German business transaction code ("Geschäftsvorfallcode"). | Reinterpreted as a signed 16-bit value after reading. This field and the next are the only `u16` fields of any record that are treated as signed (document 02, section 4.4). |
| 14 | 2 | `u16` | `gvc_extension` | Text key extension. | Reinterpreted as signed, like `gvc`. |
| 15 | 2 | `date_clamped` | `analysis_date` | Date the user assigned for evaluation, overriding the other dates in reports. | Cleared for cash accounts. |
| 16 | 2 | `date_clamped` | `transaction_date` | Date of the underlying transaction (for example the card payment). | Cleared for cash accounts. |
| 17 | 2 | `string` | `swift_transaction_code` | SWIFT transaction type code from the statement (for example `NTRF`). | |
| 18 | 2 | `decimal` | `vat_amount` | VAT contained in `amount`, without sign. | Net amount is `amount - vat_amount`. |
| 19 | 2 | `u8` | `vat_rate_type` | VAT treatment (section 11.9). | |
| 20 | 2 | `decimal` | `instructed_amount` | Original amount, if the booking was converted from another currency. | |
| 21 | 2 | `code(3)` | `instructed_currency` | Currency of `instructed_amount`. | |
| 22 | 2 | `decimal` | `charges_amount` | Charges deducted by the bank. | |
| 23 | 2 | `code(3)` | `charges_currency` | Currency of `charges_amount`. | |
| 24 | 2 | `string` | `debit_return_order_id` | Bank reference needed to return (reject) this direct debit online. | |
| 25 | 2 | `date_raw` | `debit_return_date` | Entry date that belongs to that reference. | |
| 26 | 2 | `time_hms` | `debit_return_time` | Entry time that belongs to that reference. | |
| 27 | 2 | `string` | `virtual_iban` | Virtual IBAN the payment was addressed to. | |

Reads whose result is not stored as it comes off the wire: position 9 is kept
aside until the end of 6.2; positions 13 and 14 are reinterpreted as signed;
positions 15 and 16 are validated while reading (`date_clamped`).

Not on the wire, although they exist in memory: `cross_entry_oid` (6.5),
`running_balance` and `day_balance` (6.7).

### 6.2 Adjustments after reading

Performed in this order once all fields are read. The account is the one whose
directory the file is in; `account_kind` and the account currency come from the
account record.

1. If `booking_date` is "no date" and `booking_status` is 0 (booked),
   `booking_status` becomes 1 (pending).
2. If `flag` is below 0 or above 7, it becomes 0.
3. Unless the account is a PayPal account (`account_kind` `0x20`), `currency`
   is replaced by the account's currency. If the account has no currency of its
   own, the application's default currency is used (see below).
4. If the account is a cash account (`account_kind` 2), `value_date`,
   `analysis_date` and `transaction_date` are cleared.
5. If the `batch_oid` read at position 9 is -1, it becomes 0.
6. If `split` has bit 0 or bit 4 set (a master of either kind) and `batch_oid`
   is 0, `batch_oid` becomes the record's own `oid`.
7. The result is stored as the base's `batch_oid`.

Independently of this, Banking4 always shows the account's currency for a
booking, whatever is stored. For a PayPal account that means the stored
`currency` is kept in the record but not displayed.

The application's default currency is not in the vault. Banking4 decides it
once per program run: a user setting for the region's currency if there is
one; else a parameter of the application's manifest; else the currency of the
machine's regional settings if that is EUR or CHF; else EUR. A reader cannot
know the first three and should use EUR, or better avoid the question: the
account record says whether the account has a currency of its own (document
03), and a booking of an account without one is the only case that needs the
default.

Replicate or store raw:

| Step | Reader |
|---|---|
| 1 | Replicate. It decides whether the entry counts as booked. |
| 2 | Replicate, or store raw and treat out-of-range values as "none". |
| 3 | Replicate if the output is meant to match Banking4. Needs the account record. Storing the raw `currency` alongside is harmless. |
| 4 | Replicate if the output is meant to match Banking4. Needs the account record. |
| 5, 6, 7 | Replicate. Without step 6 an old split master does not point at itself and the batch rules of 5.10 fail. |
| `gvc` sign | Replicate (values of 32768 and above become negative), or store the raw unsigned value; real codes are three-digit numbers. |

### 6.3 Writer

The writer emits the base (5.9), calls "go to next chunk" (which closes chunk 1
with the continuation bit and opens chunk 2) and writes the 27 fields in the
order of 6.1. Differences from the read side:

- Position 9 is written from the base's `batch_oid`, that is after steps 5 and
  6 and after any reset described in 6.4.
- `analysis_date` and `transaction_date` are written as plain `date_raw` values of
  the (already validated) in-memory dates.
- `gvc` and `gvc_extension` are written as the low 16 bits of the signed value,
  which restores the bytes that were read.
- `currency` is written from the stored field, which after step 3 holds the
  account currency for all but PayPal accounts.

Nothing is read that is not written, and nothing is written that is not read.

### 6.4 Splits and batches

A booking can be the *master* of a group of other bookings in the same file,
its *slaves*:

- **Split**: the user divided one bank booking into parts with different
  categories. Master `split` = 1, slaves `split` = 2.
- **Batch**: one collective bank booking was matched with a batch of individual
  payments. Master `split` = `0x10`, slaves `split` = `0x20`.

Representation on the wire:

- Master and slaves are all ordinary booking records in `Acct<oid>/Stmt`.
- The master's `batch_oid` is its own `oid`. Every slave has its own distinct
  `oid` and carries the master's `oid` in `batch_oid`.
- A slave is created with the master's booking date, value date and status and
  has its own amount, direction, category, note, flag and VAT values. A split
  slave also copies the master's counterparty fields; a batch slave takes them
  from the payment it stands for.
- For a split, the signed values of the slaves add up to the signed value of
  the master.
- The position of a slave within its group (`batch_index`, 0 for the first
  slave) is not stored.

Money must be counted once, and there is only one right way to do it:
**count the members (the slaves) and the ordinary bookings, and leave out
every master that has members.** That is what Banking4's running balance
does: it counts every booking that is not a master and skips masters.

Counting the masters and skipping their members instead is **not
equivalent**. For a split that was written correctly the two sums agree,
because the parts add up to the master. For a batch nothing makes them
agree: the master is the bank's collective booking, the members stand for
the individual payments, each with an amount and a direction of its own, and
Banking4 checks one against the other neither when it reads nor when it
writes. In the booking fixtures the batch master `batch_master` has a signed
value of -1027.40 while its three members (`batch_slave_1` to
`batch_slave_3`) add up to +1030.14: the two ways of counting differ in
amount and in direction. An earlier version of this section allowed either;
that was wrong.

`b4` therefore counts members and ordinary bookings and leaves out masters
that have members, in every sum, in the booking summary and in the view it
offers for queries. "Master" means the role after the regrouping below: a
record stored as a master that ends up without a member is reset to an
ordinary booking in step 3 and counts.

After a file has been loaded completely, Banking4 sorts the bookings and
re-derives the groups. To show the same grouping a reader has to do the same:

1. Sort ascending by: sort date; then `batch_oid` if the record is in a batch,
   else its own `oid`; then, among records that tie, a master before its
   slaves; then `oid`. The sort date of a booking is `booking_date`, or today's
   date if it has none; for a booking that is not booked (status 1 or 2) the
   last instant of that day is used, so such entries sort after the booked ones
   of the same day.
2. Walk the sorted list. Each record first gets `batch_index` 0. A master
   becomes the current master. Each directly following record with the same
   `batch_oid` gets the next `batch_index` (0, 1, 2, ...). The group ends at
   the first record with a different `batch_oid`.
3. A master that ends up with no slave, and a slave that is not reached through
   a preceding master, are reset: `batch_oid` becomes 0 and `split` becomes 0.

Step 1 depends on the current date for entries without a booking date. That
only affects the relative order of such entries, not the grouping of dated
ones.

The same sort and regrouping is applied to payments (section 8), with
`scheduled_date` (or today if empty) as sort date and without the end-of-day
rule.

Standing orders are sorted with the same key but not regrouped, and their sort
date is different: it is the next execution date Banking4 reports (9.2). For a
local standing order that is the stored `next_execution_date`; for any other
it is computed from the schedule and the current date. A standing order
without such a date (none stored, or the schedule has run past
`last_execution_date`) is not given today's date; it sorts before all others.
The order of standing orders in Banking4 therefore changes with the calendar,
and a reader is better off choosing an order of its own.

One input makes Banking4's sort fail outright: a booking that is not booked
(status 1 or 2) and dated 9999-12-31. The last instant of that day cannot be
computed, and the load of the file stops with an error. No real statement
contains such an entry, but 9999-12-31 is a legal `date_raw` (it is
Banking4's "end of time"), so a damaged or crafted file can.

A reader must not fail here, and one such record must not cost the other
bookings of the file. **Sort keys saturate:** every step that computes a
sort key from a date (the last instant of a day, a day added, a date turned
into a number) uses saturating arithmetic, so the key of a pending booking
dated 9999-12-31 is the largest key there is. The entry sorts last and
everything else is unaffected. The same holds for the sort keys of payments
and standing orders.

### 6.5 Cross entries

Banking4 can mark two bookings in different accounts as two sides of one
internal transfer. The link (`cross_entry_oid`, the `oid` of the partner
booking) exists only in memory. It is not in the booking record and not in any
other field described here. It is found again at run time by a heuristic over
all accounts (opposite directions, equal amounts, dates at most seven days
apart, different accounts, plus a text comparison that is not specified here).

What does persist is a side effect: when a pair is marked and a booking has no
category, it is given the full name of the built-in "transfer" category
(`(Umbuchung)` in German, `(Transfer)` in English, `(Transfert)` in French),
and that name is stored in `category` like any other. That category is the
special category "reclassification" of document 05, section 4.3, which is
where the names are listed; in the catalogue it is recognised by its
`special_category` value, not by its name.

A reader cannot recover the partner of a transfer from the vault. It can only
recognise the category.

### 6.6 Displayed transaction code

The stored `transaction_code` is a fallback. For display Banking4 computes a
code from other fields and uses the stored one only if that computation yields
0 (other):

- bank accounts (`account_kind` 1): from `purpose_code`; if that gives 0 and
  `gvc` is not 0, from `gvc` and `gvc_extension`; if that still gives 0 and
  `creditor_id` has a value, 2 (debit);
- PayPal accounts: from `booking_text` and the direction;
- credit card accounts (`account_kind` 4): if the stored code is 0, 9 (card
  payment) for a debit and 8 (cash management) for a credit;
- all other accounts: the stored code.

The lookup tables behind the first two cases are not given here (section
14). A reader should store the raw value.

### 6.7 Balances

`running_balance` (balance after the entry) and `day_balance` (balance at the
end of the entry's day) are not stored. They are recomputed from the statement
balance kept in the account record: pending entries dated on or after the
balance date are added first, then the sorted bookings are walked backwards
and the signed value of every booking that is not a master is subtracted.

**`b4` does not reproduce these two values.** It has no running balance
column, no day balance column and no view that computes one. The reason is
that the paragraph above is all that is known, it is not confirmed (13.5), and
it leaves open what an implementation would have to decide:

- which date of a pending entry is compared with the balance date (booking
  date or value date), and what happens to one without a date;
- whether "added" means the signed value, and whether the pending entries
  that were added first are subtracted again in the backward walk;
- where the walk stops, and whether bookings dated after the balance date
  take part in it;
- which balance date is meant when the account has both the day-precision
  and the tick-precision time (03, account fields 20 and 51).

Two readers that both follow the sentence can arrive at different balances
for the same statement, and a balance that is wrong by one pending entry
looks exactly like a right one. What the vault does hold is exported as it
is: the account's `statement_balance` with its time, and every booking with
its signed amount, status and role. The only confirmation is on the
demo vault (`tests/fixtures/vault/`), a test file produced with Banking4:
for it Banking4 derives a running balance of 0.00 before the first entry of
every account. In that vault each stored balance is the sum of the booked
entries that are not masters. That is a plausibility check of the backward
walk, not a specification.

## 7. Split editing row

Not stored in any vault file (section 1). The layout is given for completeness
because Banking4 can read and write such a record. Single chunk.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object header, as in section 4. | |
| 2 | 1 | `i32` | `update_stamp` | Object header. | |
| 3 | 1 | `bool` | `debit_flag` | Direction of this part. | A full byte here, unlike the bit in the payment base. |
| 4 | 1 | `decimal` | `amount` | Amount of this part, without sign. | |
| 5 | 1 | `string` | `category` | Category of this part. | Normalised as in 5.7. |
| 6 | 1 | `string` | `notes` | Note of this part. | |
| 7 | 1 | `i32` | `flag` | Colour flag (section 11.6). | Not range-checked here. |
| 8 | 1 | `ticks` | `analysis_date` | Evaluation date. | A timestamp here, a `date_clamped` in the booking. |
| 9 | 1 | `decimal` | `vat_amount` | VAT contained in `amount`. | |
| 10 | 1 | `u8` | `vat_rate_type` | VAT treatment (section 11.9). | |

The writer emits the same ten values in the same order; `category` is written
as held in memory, as in 5.9 *(not confirmed for this record)*.

## 8. Payment

File `Acct<oid>/Paymts`. Object header, payment base, then:

### 8.1 Read order

"Go to next chunk" is called first, with the same effect as for bookings.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 2 | `u16` | `state` | Processing state, a bit set (section 11.10). | Legacy values are remapped while loading, see 8.3. |
| 2 | 2 | `u16` | `flags` | Bit set (section 11.11). | Legacy bits are removed while loading, see 8.3. |
| 3 | 2 | `i32` | `outbox_oid` | `oid` of the outbox order this payment was submitted with. | 0 or -1 if none. |
| 4 | 2 | `i32` | `reserved_a` | Unused. | Read and discarded. Written as 0. |
| 5 | 2 | `i32` | `batch_oid` | `oid` of the batch master. | -1 becomes 0, then stored in the base (5.10). |
| 6 | 2 | `i32` | `color_flag` | Colour flag (section 11.6). | Not range-checked here. |
| 7 | 2 | `string` | `initiator_name` | Name of the initiating party of a batch. | |
| 8 | 2 | `u8` | `batch_booking` | Whether a batch is to be booked as one entry (section 11.12). | |
| 9 | 2 | `string` | `used_tan` | TAN that authorised the order. | Authentication material; see the note below. |
| 10 | 2 | `string` | `import_id` | Identifier assigned when the payment was imported from a file. | |
| 11 | 2 | `ticks` | `reserved_b` | Unused. | Read and discarded. Written as 0. |
| 12 | 2 | `i32` | `reconciled_booking_oid` | `oid` of the booking in the same account's `Stmt` that this payment was matched with. | 0 if none. |
| 13 | 2 | `string` | `order_reference_tan` | Order reference for a pending TAN procedure. | Authentication material. |
| 14 | 2 | `string` | `payment_reference` | Internal reference generated when the payment was created. | |
| 15 | 2 | `string` | `payee_verification_json` | Result of the verification of payee check for this payment, as JSON text. | Parsed into an object on load, and a text Banking4 cannot parse makes it reject the record, see 8.5. Store the text. |
| 16 | 2 | `string` | `sepa_status_code` | Four-letter status code from a payment status report (section 11.13). | Converted to a number on load: "no value" becomes 0, an unrecognised text becomes -1. |
| 17 | 2 | `string` | `sepa_reason_code` | Reason code from a payment status report. | |
| 18 | 2 | `string` | `sepa_additional_info` | Additional information from a payment status report, lines separated by LF. | Read and then lost, see 8.2. |
| 19 | 2 | `bool` | `reserved_c` | Unused. | Read and discarded. Written as false. |
| 20 | 2 | `u8` | `payee_verification_choice` | The user's choice for the verification of payee (section 11.14). | |

`used_tan` and `order_reference_tan` are one-time authorisation values. They
are bank access material: `b4` reads them only to stay in step and drops
them at decode time, so they are never kept in a memory structure, never
written to the database and never exported (document 03, "Entries").

### 8.2 Adjustments after reading

1. `batch_oid`: -1 becomes 0. The result is stored in the base. Unlike for
   bookings, nothing repairs a master here.
2. `sepa_additional_info` is read and split into lines (as in 5.6), but the
   result is never stored: the field keeps what it held before, which for a
   record being read is nothing. The in-memory field is therefore absent
   after loading, whatever the record contains. This looks like a defect in
   Banking4. A reader that stores the raw text has *more* information than
   Banking4 shows.
3. `payee_verification_json` is parsed (8.5).

### 8.3 Adjustments by the collection

While the file is loaded, each decoded payment is post-processed before it is
added to the list. Let `legacy_batch_bits` be bits `0x1C` of `flags`.

1. A payment whose `state` is exactly `0x80` is dropped: it does not appear in
   Banking4 at all.
2. `state` exactly `0x10` becomes `0x01` (local). `state` exactly `0x04`
   becomes `0x02` (in outbox).
3. If bit `0x08` of `flags` (legacy batch master) is set, the payment is
   remembered under its current `batch_oid`, and its `batch_oid` is then set to
   its own `oid`. That assignment counts as a change of the record: in memory
   its `update_stamp` becomes -1 ("not stamped yet") until it is saved. If a
   second legacy master carries a `batch_oid` that is already remembered, the
   load of the file stops there with an error.
4. `legacy_batch_bits` are cleared in `flags`.
5. After the whole file is read, and only if step 3 found at least one legacy
   master: every payment that is a slave by the rule of 5.10 and whose
   `batch_oid` matches a remembered value gets the `batch_oid` of that master
   (its `oid`), and also takes over the master's `scheduled_date` and the
   master's reported `scheduled_time` (which is "no time" unless the master's
   `scheme_type` is 8, see 5.3).
6. Sort and regroup as in 6.4.

A reader must replicate steps 1, 2 and 4 to show the same list and states.
Steps 3 and 5 concern a batch representation that current versions no longer
write; a reader can replicate them or reject records with bit `0x08` set.

### 8.4 Writer

Base (5.9), "go to next chunk", then the 20 values in the order of 8.1.
`reserved_a` is written as 0, `reserved_b` as 0 ticks, `reserved_c` as false.
`batch_oid` is written from the base. `sepa_status_code` is written as the
four-letter code of the in-memory number, or "no value" if the number is 0, -1
or any other number without a code, so an unrecognised status text does not
survive a load and save. `sepa_additional_info` is written from the in-memory
lines, which after a load are absent (8.2), so loading and saving a file
erases that field. `payee_verification_json` is written by serialising the
in-memory object again (8.5), not from the text that was read.

### 8.5 Payee verification text

`payee_verification_json` is "no value" for most payments. Otherwise it is the
text of one JSON object. Banking4 parses it while reading the record and keeps
an object with these members, all optional:

| JSON member | JSON type | Meaning |
|---|---|---|
| `vopTransactionStatus` | string | Outcome of the check, one of the codes of section 11.23. |
| `vopNotApplicableReasonCode` | string | Why the check was not applicable, one of the codes of section 11.23. |
| `vopNotApplicableReasonAdditionalInfo` | string | Free text that goes with the reason. |
| `vopCreditorName` | string | The payee name the bank reported back (the corrected name for a close match). |
| `originalCreditorName` | string | The payee name that was submitted. |
| `originalEndToEndId` | string | End-to-end reference of the submitted payment. |
| `originalCreditorAccountIBAN` | string | IBAN of the submitted payment. |
| `originalAmount` | string | Amount of the submitted payment as a decimal number with a point, for example `"13.58"`. |
| `originalPaymentInformationIndex` | number | Position of the payment's batch within the submitted file. |
| `originalTransactionInformationIndex` | number | Position of the payment within its batch. |
| `originalTransactionIndex` | number | Position of the payment within the whole file. |

What Banking4 does with the text when it reads the record:

- "No value" gives no object.
- A text that is not JSON, or is JSON but not an object (the fixtures have
  a text of blanks only, a plain text and a JSON array), makes Banking4
  reject this record. So does a status or a reason that is a text other
  than the known codes (the comparison is case-sensitive), a status that is
  not a text, and an amount that is a text but not a number. As for every
  failure inside a record (document 02), Banking4 then stops loading the
  payments file at that record.
- An empty object `{}` is accepted and gives an object with nothing set. An
  empty or `null` status is accepted as "no status".
- Members other than the eleven above are ignored.
- The member types are not enforced. Banking4 accepts an amount given as a
  JSON number, takes a name given as a number as its digits, converts an
  index given as a string of digits, cuts an index with a fraction to its
  integer part, and reads an index that is `null` or not a number as 0.
  These conversions belong to Banking4's JSON parsing and are not specified
  further.

The writer serialises the object again. The result is equivalent but not the
same text: members appear in the order of the table, separated by a comma and
a blank, inside `{ ` and ` }`; a member whose string is empty, an amount of
zero and an index that is not set are left out; members the reader ignored
are gone. An in-memory object whose status is none of the known ones cannot be
written at all (the writer fails).

A reader should keep the text and must not reject the record because of it.
If it parses the text, it should treat everything as optional.

### 8.6 Amount and direction

As in 5.5: the direction of a payment follows from `scheme_type`. Credit
transfers of every kind and "user expense" are debits, direct debits and "user
income" are credits.

## 9. Standing order

File `Acct<oid>/Stos`. Object header, payment base, then:

### 9.1 Read order

"Go to next chunk" is called first, with the same effect as for bookings.

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 2 | `i32` | `state` | Bit set (section 11.15). | |
| 2 | 2 | `i32` | `outbox_oid` | `oid` of the outbox order that set up, changed or cancelled it. | |
| 3 | 2 | `date_raw` | `first_execution_date` | First execution. | |
| 4 | 2 | `i32` | `period_unit` | 77 month, 87 week, 0 unset (section 11.16). | The values are the ASCII codes of `M` and `W`. |
| 5 | 2 | `i32` | `period_length` | Number of units between executions. | Valid range 1..12 for months, 1..52 for weeks. |
| 6 | 2 | `i32` | `execution_day` | Day within the period. | Months: 1..31 is the day of the month (shortened to the month's length), 97..99 count from the end (99 last day, 98 second to last, 97 third to last). Weeks: 1 Monday .. 7 Sunday, 0 unspecified. |
| 7 | 2 | `date_raw` | `last_execution_date` | Last execution, empty for "until further notice". | |
| 8 | 2 | `date_raw` | `next_execution_date` | Next execution as computed when the record was saved. | Discarded on load unless bit 0 of `state` is set, see 9.2. |
| 9 | 2 | `i32` | `reserved_a` | Unused. | Read and discarded. Written as 0. |
| 10 | 2 | `bool` | `reserved_b` | Unused. | Read and discarded. Written as false. |
| 11 | 2 | `i32` | `reserved_c` | Unused. | Read and discarded. Written as 0. |
| 12 | 2 | `ticks` | `reserved_d` | Unused. | Read and discarded. Written as 0. |
| 13 | 2 | `time_hms` | `execution_time` | Execution time of day. | |

There is no `batch_oid`. The in-memory value keeps its initial -1.

### 9.2 Adjustments after reading

If bit 0 (`local`) of `state` is clear, `next_execution_date` is set to "no
date". For such standing orders (those held at the bank, and those Banking4
detected automatically from bookings) the next date shown in the application is
computed from the schedule and the current date instead.

A reader should store the raw date and the state and apply the rule when
presenting; the stored date of a non-local standing order is stale by
definition.

### 9.3 Writer

Base (5.9), "go to next chunk", then the 13 values in the order of 9.1 with the
four reserved values written as 0, false, 0 and 0 ticks. `next_execution_date`
is written from memory, so after a load and save a non-local standing order has
"no date" there.

### 9.4 Amount and direction

As in 5.5: from `scheme_type`, which for standing orders is typically 1 (user
expense), 2 (user income) or 5 (SEPA credit transfer).

## 10. Outbox order

File `Outbox` at the top level of the vault. Same collection framing, single
chunk per record. It is **not** stored the way the payment family is: it does
not use the payment base, has no second chunk, and contains two counted lists.

### 10.1 Read order

| Position | Chunk | Wire primitive | Own field name | Meaning | Notes |
|---|---|---|---|---|---|
| 1 | 1 | `i32` | `oid` | Object header, as in section 4. | Referenced by `outbox_oid` of payments and standing orders. |
| 2 | 1 | `i32` | `update_stamp` | Object header. | |
| 3 | 1 | `i32` | `state` | Processing state (section 11.17). | |
| 4 | 1 | `i32` | `order_type` | Kind of order (section 11.18). | |
| 5 | 1 | `i32` | `account_oid` | `oid` of the ordering account. | |
| 6 | 1 | `date_raw` | `created_date` | Date the order was placed in the outbox. | Sort key together with `created_time`. |
| 7 | 1 | `time_hms` | `created_time` | Time the order was placed in the outbox. | |
| 8 | 1 | `string` | `order_text` | Human readable summary of the order. | |
| 9 | 1 | `decimal` | `amount` | Order amount. | No direction, see 5.5. |
| 10 | 1 | `string` | `order_data_xml` | Order payload as XML text. | Kept only while `state` is -1, 0 or 1 and the string is not empty; dropped otherwise. Current versions keep the payload in `OutboxData/<oid>.xml` instead. |
| 11 | 1 | `date_raw` | `transmitted_date` | Date the order was sent. | |
| 12 | 1 | `time_hms` | `transmitted_time` | Time the order was sent. | |
| 13 | 1 | `string` | `order_reference` | Reference returned by the bank. | If two orders in the file carry the same reference, the later one loses it while loading. |
| 14 | 1 | `i32` | `response_count` | Number of response texts that follow. | |
| 15 | 1 | `string` | `response_texts[]` | One bank response text. | Repeated `response_count` times. |
| 16 | 1 | `code(3)` | `currency` | Currency of `amount`. | |
| 17 | 1 | `string` | `order_id` | Order id assigned by the bank. | |
| 18 | 1 | `bool` | `has_unseen_protocol` | Protocol entries were added since the user last looked. | |
| 19 | 1 | `i32` | `protocol_count` | Number of protocol entries that follow. | |
| 20 | 1 | `ticks` | `protocol[].timestamp` | Time of the protocol entry. | Positions 20 to 22 repeat together `protocol_count` times. |
| 21 | 1 | `u8` | `protocol[].state` | State the order had at that time (section 11.17). | Read as an unsigned byte, so a stored `FF` comes back as 255, not as the -1 that means "sending" in the order's own `state`: that state does not round-trip through a protocol entry. |
| 22 | 1 | `string` | `protocol[].text` | Protocol text. | |
| 23 | 1 | `string` | `used_tan` | TAN that authorised the order. | Authentication material. |
| 24 | 1 | `string` | `account_iban` | IBAN of the ordering account. | |
| 25 | 1 | `string` | `contact_name` | Name of the bank contact the order was sent through. | Bank access data. |
| 26 | 1 | `string` | `contact_user_id` | User id of that bank contact. | Bank access data. |
| 27 | 1 | `i32` | `target_account_oid` | `oid` of the receiving own account, for internal transfers. | |
| 28 | 1 | `string` | `target_account_iban` | IBAN of the receiving own account. | |
| 29 | 1 | `string` | `order_reference_tan` | Order reference for a pending TAN procedure. | Authentication material. |
| 30 | 1 | `bool` | `needs_more_authorisation` | The bank requires further signatures. | |
| 31 | 1 | `bool` | `priority_order` | Order is to be sent before others. | |
| 32 | 1 | `string` | `payee_verification_json` | Result of the verification of payee check, as JSON text. | Parsed on load if not empty. Store the text. |
| 33 | 1 | `bool` | `payee_verification_opt_in_preferred` | User preference for the check. | |
| 34 | 1 | `u8` | `payee_verification_option` | 0 none, 1 opt in, 2 opt out (section 11.20). | |
| 35 | 1 | `guid` | `contact_guid` | Identifier of the bank contact used. | Bank access data. |
| 36 | 1 | `i32` | `contact_protocol` | Protocol used (section 11.19). | |
| 37 | 1 | `string` | `contact_order_type` | Protocol-specific order type that was used. | |

Neither count is checked against the size of the record by Banking4. A
negative count reads nothing; a count larger than the data yields
that many "no value" entries. A reader must not follow it there: never
allocate by the count; refuse a count larger than the bytes left in the
chunk divided by the smallest size of one element, which is 2 bytes for a
response text (a `string`) and 11 bytes for a protocol entry (`ticks`, `u8`
and a `string`: 8 + 1 + 2); make that comparison as a division or in checked
arithmetic, never as `count * 11` in 32 bits, which wraps for counts around
200 million and then lets the count through.

The outbox order links a payment or standing order to the bank contact it was
sent through. `contact_name`, `contact_user_id`, `contact_guid`, `used_tan` and
`order_reference_tan` are bank access data. For that reason `Outbox` and
`OutboxData` are among the entries `b4` never reads (document 03, "Entries"):
this section documents the layout and is not implemented in `b4`.

### 10.2 Writer

Same 37 positions in the same order, both counts written from the list lengths.
One difference: `order_data_xml` is written from the in-memory field, which the
reader only fills for states -1, 0 and 1, so a payload stored with a completed
order disappears on the next save.

## 11. Enumerations

"Flags" marks a bit set: several values can be combined. "Alias" marks a name
for a combination of other values, not a value of its own.

### 11.1 `scheme_class` (4 bits)

| Value | Name |
|---|---|
| 0 | `any` |
| 1 | `sepa` |
| 2 | `foreign_legacy` (German DTAZV format) |
| 3 | `domestic_legacy` (German DTAUS format) |
| -1 | `unknown`; never derived from a scheme type, and not representable in four bits other than as 15 |

### 11.2 `scheme_type` (`i32`)

| Value | Name | Class written | Direction |
|---|---|---|---|
| 0 | `none` | 0 | from `debit_flag` |
| 1 | `user_expense` | 0 | debit |
| 2 | `user_income` | 0 | credit |
| 3 | `domestic_legacy_credit_transfer` | 3 | debit |
| 4 | `foreign_legacy_credit_transfer` | 2 | debit |
| 5 | `sepa_credit_transfer` | 1 | debit |
| 6 | `sepa_direct_debit_core` | 1 | credit |
| 7 | `sepa_direct_debit_b2b` | 1 | credit |
| 8 | `sepa_instant_credit_transfer` | 1 | debit |
| 9 | `urgent_credit_transfer` | 1 | debit |
| 10 | `foreign_credit_transfer` | 1 | debit |
| 11 | `swiss_credit_transfer` | 1 | debit |

Any other value is treated as class 0 and debit.

### 11.3 `cost_bearer` (`u8`)

| Value | Name |
|---|---|
| 0 | `none` |
| 1 | `ours` (`OUR`) |
| 2 | `shared` (`SHA`) |
| 3 | `beneficiary` (`BEN`) |

### 11.4 `booking_status` (`u8`)

| Value | Name | Statement report code |
|---|---|---|
| 0 | `booked` | `BOOK` |
| 1 | `pending` | `PDNG` |
| 2 | `information` | `INFO` |

### 11.5 `entry_bits` (`u8`, flags)

| Value | Name |
|---|---|
| `0x01` | `read` |
| `0x02` | `exported_to_books` |
| `0x04` | `uploaded` (to a document service) |

### 11.6 `flag` / `color_flag` (`i32`)

| Value | Name |
|---|---|
| -1 | `undefined`; in-memory sentinel |
| 0 | `none` |
| 1 | `ok` |
| 2 | `red` |
| 3 | `blue` |
| 4 | `yellow` |
| 5 | `green` |
| 6 | `gray` |
| 7 | `purple` |
| 8 | count sentinel, not a flag |

### 11.7 `transaction_code` (`i32`)

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
| 10 | count sentinel, not a code |

### 11.8 `split` (`u8` on the wire, flags)

| Value | Name | Kind |
|---|---|---|
| `0x00` | `none` | |
| `0x01` | `split_master` | |
| `0x02` | `split_slave` | |
| `0x10` | `batch_master` | |
| `0x20` | `batch_slave` | |
| `0x11` | `any_master` | alias, used as a mask in 6.2 step 6 |
| `0x22` | `any_slave` | alias |
| `0xFF` | `type_mask` | alias |
| `0x100` | `first_in_group` | in memory only; does not fit the stored byte |
| `0x200` | `last_in_group` | in memory only; does not fit the stored byte |

### 11.9 `vat_rate_type` (`u8`, flags)

| Value | Name |
|---|---|
| `0x00` | `none` |
| `0x01` | `standard` |
| `0x02` | `reverse_charge` |
| `0x04` | `non_taxable` |
| `0x08` | `vat_accounting` |
| `0xFF` | `any` (alias, mask) |

### 11.10 Payment `state` (`u16`, flags)

| Value | Name | Kind |
|---|---|---|
| `0x0000` | `none` | |
| `0x0001` | `local` | entered, not submitted |
| `0x0002` | `in_outbox` | |
| `0x0004` | `legacy_postdated_in_outbox` | legacy, remapped in 8.3 |
| `0x0008` | `cancel_postdated_in_outbox` | |
| `0x0010` | `legacy_advised_payment` | legacy, remapped in 8.3 |
| `0x0020` | `postdated_at_bank` | |
| `0x0040` | `sent` | |
| `0x0080` | `legacy_advised_direct_debit` | legacy, record dropped in 8.3 |
| `0x0100` | `orphaned_postdated` | |
| `0x0200` | `reconciled` | matched with a booking |
| `0x0043` | `exportable` | alias |
| `0x0160` | `all_remitted` | alias |
| `0x0161` | `reconcilable` | alias |

### 11.11 Payment `flags` (`u16`, flags)

| Value | Name | Kind |
|---|---|---|
| `0x0001` | `warning` | |
| `0x0002` | `legacy_delete` | legacy |
| `0x0004` | `legacy_batch_slave` | legacy, cleared in 8.3 |
| `0x0008` | `legacy_batch_master` | legacy, cleared in 8.3 |
| `0x0010` | `legacy_batch_expanded` | legacy, cleared in 8.3 |
| `0x001C` | `legacy_batch_bits` | alias |
| `0x0100` | `needs_more_authorisation` | |
| `0x1000` | `confirmed_to_accounting` | |

### 11.12 `batch_booking` (`u8`)

| Value | Name |
|---|---|
| 0 | `default` |
| 1 | `yes` |
| 2 | `no` |
| 3 | `mixed` |

### 11.13 `sepa_status_code` (text on the wire)

| Text | Number in memory | Name |
|---|---|---|
| `RCVD` | 1 | `received` |
| `PDNG` | 2 | `pending` |
| `PART` | 3 | `partially_accepted` |
| `ACTC` | 4 | `accepted_technical_validation` |
| `ACCP` | 5 | `accepted_customer_profile` |
| `ACWC` | 6 | `accepted_with_change` |
| `ACSP` | 7 | `accepted_settlement_in_process` |
| `ACCC` | 8 | `accepted_settlement_completed_creditor` |
| `ACSC` | 9 | `accepted_settlement_completed_debtor` |
| `RJCT` | 10 | `rejected` |
| (no value) | 0 | no status |
| (any other text) | -1 | `unknown` |

### 11.14 `payee_verification_choice` (`u8`)

| Value | Name |
|---|---|
| 0 | `default` |
| 1 | `opt_in` |
| 2 | `opt_out` |

### 11.15 Standing order `state` (`i32`, flags)

| Value | Name | Kind |
|---|---|---|
| `0x00000` | `none` | |
| `0x00001` | `local` | kept in Banking4 only |
| `0x00002` | `setup_in_outbox` | |
| `0x00004` | `change_in_outbox` | |
| `0x00008` | `cancel_in_outbox` | |
| `0x00010` | `at_bank` | |
| `0x00020` | `auto_detected` | derived by Banking4 from recurring bookings |
| `0x10000` | `warning` | |
| `0x20000` | `deleted` | |
| `0x0FFFF` | `state_mask` | alias |
| `0x00021` | `editable` | alias (`local` or `auto_detected`) |

The alias `0x00021` exists under two names with the same meaning.

### 11.16 `period_unit` (`i32`)

| Value | Name |
|---|---|
| 0 | `none` |
| 77 | `month` |
| 87 | `week` |

### 11.17 Outbox `state` (`i32`; `u8` in protocol entries)

| Value | Name |
|---|---|
| -1 | `sending` |
| 0 | `none` |
| 1 | `active` (waiting to be sent) |
| 2 | `succeeded` |
| 3 | `succeeded_with_info` |
| 4 | `succeeded_with_warning` |
| 5 | `failed` |
| 6 | `deleted` |
| 7 | `indeterminate` |
| 8 | `received` |
| 9 | `waiting_for_signature` |
| 10 | `cancelled` |

### 11.18 Outbox `order_type` (`i32`)

Values marked "unsupported" are order kinds of older versions that current
versions can still display but not create.

| Value | Name |
|---|---|
| 0 | `none` |
| 1 | `domestic_single_transfer` (unsupported) |
| 2 | `domestic_single_direct_debit` (unsupported) |
| 3 | `domestic_reclassification` (unsupported) |
| 4 | `euro_payment` (unsupported) |
| 5 | `domestic_submit_postdated_transfer` (unsupported) |
| 6 | `domestic_cancel_postdated_transfer` (unsupported) |
| 7 | `domestic_setup_standing_order` (unsupported) |
| 8 | `domestic_modify_standing_order` (unsupported) |
| 9 | `domestic_cancel_standing_order` (unsupported) |
| 10 | `domestic_single_direct_debit_v4` (unsupported) |
| 11 | `sepa_single_transfer` |
| 12 | `domestic_multi_transfer` (unsupported) |
| 13 | `domestic_multi_direct_debit` (unsupported) |
| 14 | `foreign_transfer` |
| 15 | `prepaid_load` |
| 16 | `customer_message` |
| 17 | `sepa_single_direct_debit` |
| 18 | `sepa_multi_transfer` |
| 19 | `sepa_multi_direct_debit` |
| 20 | `sepa_postdated_single_transfer` (unsupported) |
| 21 | `sepa_setup_standing_order` |
| 22 | `sepa_modify_standing_order` |
| 23 | `sepa_cancel_standing_order` |
| 24 | `sepa_cancel_postdated_transfer` |
| 25 | `sepa_cancel_postdated_direct_debit` |
| 26 | `foreign_multi_transfer` |
| 27 | `sepa_reclassification` (transfer between own accounts) |
| 28 | `sepa_setup_standing_direct_debit` |
| 29 | `sepa_modify_standing_direct_debit` |
| 30 | `sepa_cancel_standing_direct_debit` |
| 31 | `signature_folder_sign` |
| 32 | `signature_folder_cancel` |
| 33 | `sepa_instant_payment` |
| 34 | `swiss_transfer_ch01` (unsupported) |
| 35 | `swiss_transfer_ch02` (unsupported) |
| 36 | `swiss_transfer` |
| 37 | `sepa_urgent_payment` |
| 38 | `sepa_single_direct_debit_b2b` |
| 39 | `sepa_multi_direct_debit_b2b` |
| 40 | `foreign_transfer_iso20022` |
| 41 | `foreign_multi_transfer_iso20022` |
| 42 | `sepa_setup_standing_direct_debit_b2b` |
| 43 | `sepa_modify_standing_direct_debit_b2b` |
| 44 | `sepa_cancel_standing_direct_debit_b2b` |
| 45 | `sepa_cancel_postdated_direct_debit_b2b` |
| 46 | `sepa_reject_debit` |
| 47 | `acknowledge` |
| 48 | `sepa_multi_instant_payment` |
| 49 | `sepa_instant_reclassification` |
| 50 | `sepa_setup_standing_instant_payment` |
| 51 | `sepa_modify_standing_instant_payment` |
| 52 | `sepa_cancel_standing_instant_payment` |
| 53 | `sepa_cancel_postdated_instant_payment` |

### 11.19 `contact_protocol` (`i32`, flags)

| Value | Name |
|---|---|
| `0x0000` | `undefined` |
| `0x0001` | `fints` |
| `0x0002` | `ebics` |
| `0x0004` | `paypal` |
| `0x1000` | `screen_scraping` |
| `0x2000` | `spooler_folder` |

### 11.20 `payee_verification_option` (`u8`)

| Value | Name |
|---|---|
| 0 | `none` |
| 1 | `opt_in` |
| 2 | `opt_out` |

### 11.21 Record kind (not on the wire)

The base reader's last step (5.4) branches on the kind of the concrete record.
The kind is a property of the record type, computed, never stored:

| Value | Name | Which records |
|---|---|---|
| `0x0000` | `none` | |
| `0x0001` | `expense` | payment with debit direction |
| `0x0002` | `income` | payment with credit direction |
| `0x0010` | `standing_expense` | standing order with debit direction |
| `0x0020` | `standing_income` | standing order with credit direction |
| `0x0100` | `statement_entry` | booking |
| `0x1000` | `signature_folder_item` | |
| `0x0003` | `payment` | alias |
| `0x0030` | `standing_order` | alias |
| `0xFFFF` | `all` | alias |

### 11.22 `account_kind` values used by the rules above

The account kind belongs to the account record and is specified in document
03. The rules in this file use: 1 bank, 2 cash, 4 credit card, `0x20` PayPal.

### 11.23 Codes in the payee verification text (8.5)

`vopTransactionStatus`:

| Text | Number in memory | Name |
|---|---|---|
| `RCVC` | 1 | `match` |
| `RVMC` | 2 | `close_match` |
| `RVNM` | 3 | `no_match` |
| `RVNA` | 4 | `not_applicable` |
| `PDNG` | 5 | `pending` |

`vopNotApplicableReasonCode`:

| Text | Number in memory | Name |
|---|---|---|
| `AG03` | 1 | `transaction_not_supported` |
| `AB11` | 2 | `timeout_by_debtor_agent` |

## 12. Reader checklist

What a reader has to do beyond decoding the tables, to arrive at the values
Banking4 works with.

| Topic | Section | Replicate? |
|---|---|---|
| Two-chunk versus single-chunk records | 5.2 | Must detect. Reading the extension from a single-chunk record misreads everything after it. |
| Tagged purpose lines of single-chunk records | 5.8 | Must replicate to support such records, otherwise reject them. |
| Scheme derivation | 5.4 | Must replicate for payments and standing orders; bookings always get 0. |
| Effective direction and sign | 5.5 | Must replicate. Never negate by the raw bit alone for payments and standing orders. |
| Purpose text to lines | 5.6 | Replicate, or store the raw text and drop blank lines when splitting. |
| Category normalisation | 5.7 | Replicate trimming and part repair; pseudo categories need the language. The writer does not normalise, so do not skip this for "current" records. |
| Displayed currency of payments and standing orders | 5.11 | Replicate if the output is meant to match Banking4; store the raw `currency` in any case. |
| Booking adjustments | 6.2 | Replicate; steps 3 and 4 need the account record. |
| Split and batch grouping, `batch_index` | 6.4 | Replicate sort and regrouping if groups are exposed. In sums count members and ordinary bookings and leave out masters that have members; counting masters instead is not equivalent. Sort keys saturate. |
| Cross entries | 6.5 | Cannot be replicated from the vault. |
| Displayed transaction code | 6.6 | Store raw. |
| Balances | 6.7 | Not stored, and not reproduced by `b4`: the rule is underspecified. |
| Payment collection adjustments | 8.3 | Replicate steps 1, 2 and 4. |
| Lost `sepa_additional_info` | 8.2 | Reader's choice; raw text is a superset. |
| Payee verification text | 8.5 | Keep the text. Never reject a payment because the text does not parse, although Banking4 does. |
| Standing order next date | 9.2 | Store raw plus state; apply the rule when presenting. |
| Reserved fields | 8.1, 9.1 | Skip. They occupy 4, 8 and 1 bytes (payment) and 4, 1, 4 and 8 bytes (standing order). |
| Bank access data | 8.1, 10.1 | Payment: read `used_tan` and `order_reference_tan` to stay in step and drop them at decode time. Outbox: never read the file at all. |
| Counted lists | 10.1 | Never allocate by a count; bound it by the bytes left divided by the smallest element size, in checked arithmetic. |

## 13. Verification

This section says three things:

- what the fixtures of the repository cover (13.1, 13.2). They are
  `tests/fixtures/records/booking.*`, `payment.*` and `standing_order.*`:
  synthetic records with invented values, for the Banking4 version described
  here. No vault is involved;
- what was compared with data exported from Banking4 for one real vault
  (13.4, 13.6);
- what is not confirmed (13.5).

### 13.1 Test files

1. The fixtures hold 255 synthetic records. 144 of them are records as
   Banking4 writes them, produced with Banking4, with every field at a
   recognisable made-up value. Another 111 are cases in forms Banking4 never
   writes for these record types (legacy single-chunk records with plain and
   tagged purpose lines, a first chunk without the extension, chunks that end
   early, class nibbles and enumeration values out of range, unused fields
   carrying values, malformed payee verification texts), with their expected
   results. That gives 81 bookings, 131 payments and 43 standing orders.
2. For every record the fixtures give the expected result of reading it,
   bookings for five accounts (bank, cash, credit card, PayPal, and a bank
   account without a currency), payments and standing orders for three (bank,
   PayPal, and a bank account without a currency): all in-memory fields and
   the values computed from them (effective direction, signed value, record
   kind, class, displayed currency and category, reported execution time, net
   amount). That is 927 reads; 24 of them, the three reads of eight
   payments, have a rejection as their expected result (8.5). For the 144
   records as Banking4 writes them, the values the record was written from
   stand next to it. The `*.rewritten.bin` files hold the expected result of
   writing a record again after reading it, for the records where that
   differs from the bytes read.
3. For each of the three files they give the expected list after the
   records of the file are loaded: order, discarded records, changed values
   (for what a complete load of a booking file adds to this, see 13.5).
4. The tests in `tests/records_payments.rs` decode every record of the
   fixtures with `b4` and compare the result with these values, 60,753 in
   all, and what `b4` makes of the booking file and of the payment file with
   the two lists. Where `b4` differs on purpose, the tests name each
   difference; among them: it keeps the eight payments (8.5), it takes the
   pseudo category names of all three languages as such (5.7), and it has
   no list rule for standing orders (6.4). `b4` does not write records, so
   the written forms are in the fixtures and not in its tests.

### 13.2 Confirmed

What the fixtures cover:

| What | Cases in the fixtures |
|---|---|
| Read order, primitives and chunk of every row of sections 4, 5.1, 5.3, 6.1, 8.1, 9.1; the "more chunks follow" test (5.2); all adjustments of 5.4 to 5.7, 5.10, 5.11, 6.2, 8.2, 8.5, 9.2 | The 927 reads of the 255 records, with every in-memory field and every computed value |
| Reserved positions (payment 4, 11, 19; standing order 9 to 12) are read and discarded | Records with values in those positions: the fields after them have their expected values |
| Writer rules (5.9, 6.3, 8.4, 9.3): two chunks always; class nibble and direction bit from the scheme; purpose lines joined with LF; category as held in memory; `split`, `gvc` and `batch_booking` cut to their width; reserved positions zero; status code text | The 144 records as Banking4 writes them, next to the values they were written from; for the 247 records that are not rejected, the expected result of writing the record again after reading it |
| Memory-only values are not stored: cross entry link, both balances, batch position, batch expanded, name digest | Records written while these values were not at their defaults hold nothing of them and read back as defaults |
| Scheme derivation (5.4) | Class nibbles 0, 1, 2, 3, 4, 15 with both direction bits; class 1 with local instrument absent, `INST`, `inst`, `INST` plus a blank, `B2B`, `CORE`; stored scheme types -1, 0 to 12 and 99; bookings with a stored scheme |
| Effective direction and signed value (5.5) | Every record, including payments whose direction bit contradicts their scheme and negative amounts |
| Purpose text to lines (5.6) | CR, LF, CR LF, blank lines, trailing break, blank-only lines, U+0085 and U+2028 as ordinary characters, forty lines |
| Category normalisation (5.7) | Untidy, blank and pseudo names; the expected results are those for the German texts |
| Tagged purpose lines of single-chunk records (5.8) | Every tag, repeated tags, malformed and empty dates, all four ultimate-party branches, for the three record types |
| Displayed currency (5.11, 6.2) and displayed category (5.7) | Payments without a stored currency for scheme types 0 to 12, standing orders without one for scheme types 0 to 5 and 9; bookings for the five accounts; masters with a category |
| Application default currency (6.2) | Every record has an expected result for an account without a currency, with CHF as the default. Where the default comes from is not part of the fixtures |
| Booking adjustments (6.2) | Missing booking date with each status; flags -5, -1, 8, 1000; batch ids -1, 0, own and foreign with `split` values 0, 1, 2, `0x10`, `0x20`, `0xFF` and one above the byte; cash, PayPal and no-currency accounts; validated dates with zero parts, month 13, day 40, 30 February, 31 April, year 10000, 0001-01-01 |
| Unvalidated raw dates | Year only, year and month, 31 February, month 13 and day 32 come back as stored |
| Sort and regrouping of bookings (6.4) | The expected list for the booking file, with its records in the order of the file (the merge of the legacy file of section 1, which belongs to the same load, is not part of this, see 13.5): split and batch groups, a slave on another day, masters without slaves, a self reference without a role, booked and pending entries of one day, entries without a date |
| Payment collection adjustments (8.3) with sort and regrouping | The expected list for the payment file, with its records in the order of the file: the three legacy states, the legacy flag bits, legacy masters with slaves (date and time taken over, also from a master that is not an instant transfer), a legacy slave without master, current-form groups |
| Standing orders: sorted by the reported next execution date, "no date" first, not regrouped | The expected list for the standing order file. It shows the rule for local standing orders (stored date) and for those without any further execution; for the others see 13.5 |
| Payee verification text (8.5, 11.23) | All five status codes and both reason codes with every member set; empty object; unknown members; unknown, lower-case, numeric, empty and `null` status; unknown reason; amounts as text, number, zero, not a number; indexes of the wrong type; blanks, a JSON array, plain text. Eight of these payments have a rejection as their expected result |
| `sepa_status_code` (11.13) and the loss of `sepa_additional_info` (8.2) | Every code text, an unknown and a lower-case text, none; status numbers 0 to 10, -1 and 55 on write; status report lines present in the record and gone after reading |
| Standing order next date (9.2) | Every state bit with a stored next date |
| Short chunks (section 3) | Single-chunk records, a first chunk without the extension or with the extension cut, a second chunk that is empty or ends early: missing fields read as defaults |

The fixtures hold nothing for the following, so the repository cannot show
it:

- the split editing row (section 7) and the outbox order (section 10),
  including list counts that are negative or larger than the data;
- the loading of whole files of these record types: an empty file, a
  surplus chunk, bytes after the last field of a chunk or after the last
  chunk of a record, a continuation bit with no chunk behind it, a size
  below 4, a truncated record, a duplicate `oid` (the loading of a file as
  such is the subject of document 02);
- values that stop the load of a file (section 2, last rule), and a repeated
  batch id of legacy masters (8.3 step 3);
- the sort and regrouping of records stored in another order than the
  fixture files have them;
- the category normalisation for a language other than German (5.7), and
  the sources of the application default currency (6.2);
- the text keys of `TXTS` (5.8) other than `04`, `05`, `51`, `53` and one
  key that is not a number;
- `update_stamp` -1 replaced by the current time on write, and its unit;
- the numeric values of those enumeration members of section 11 that no
  stored value of the three record types uses (the members of 11.23 are in
  the fixtures).

### 13.3 Corrections made

Earlier versions of this file were wrong or silent on the following points.

1. **The writer does not normalise the category (5.7, 5.9, section 7).** This
   file said the writer stores the normalised name. It stores the in-memory
   string; only the reader normalises. For a record that was read before it
   was written the two cannot be told apart.
2. **Displayed currency of payments and standing orders (new 5.11).** Not
   described before. Without a stored currency the reported one depends on the
   scheme: account currency, `DEM`, `EUR` or none.
3. **Application default currency (6.2).** This file said "the application
   default (EUR in the standard product)". It is decided from a user setting,
   a manifest parameter and the regional settings, in that order.
4. **Payee verification text (8.1, new 8.5, new 11.23).** This file said only
   that the text is parsed on load. A text Banking4 cannot parse makes it
   reject the record and with it the rest of the file; the members and codes
   are now listed; the writer serialises the parsed object, not the text.
5. **`sepa_additional_info` (8.2).** "Stays empty" is now "is absent", with the
   reason.
6. **Status code on write (8.4).** "No value" is written for every in-memory
   number without a code, not only for 0 and -1.
7. **Standing order sort (6.4).** This file implied the payments' sort date. It
   is the reported next execution date, and a missing date sorts first instead
   of being replaced by today.
8. **A pending booking dated 9999-12-31 (6.4)** makes Banking4's sort fail.
   New.
9. **Legacy batch masters (8.3).** Setting the master's batch id marks the
   record as changed (`update_stamp` -1 in memory). A repeated batch id stops
   the load; this used to be an open question.
10. **Raw dates (section 2).** Added that the parts are returned unvalidated
    and that a year of `0x8000` or above comes back negative. (The advice to
    treat such a year as damage, which stood here for a while, is gone: the
    read does not fail, and `b4` keeps the record, see section 2.)
11. **Saving a single-chunk record (5.9).** Added what the writer produces for
    it: a two-chunk record with the extracted fields in the extension.
12. **Name digest (5.10).** A further in-memory value that is never stored.
13. **The writer checks nothing (5.9).** Added, with the cutting of `code`
    fields and the empty string.

None of these changes the position, width or type of a stored field. The
tables of sections 4 to 9 stood as they were.

### 13.4 Field names seen in a real export

The field *names* (not the values) in data exported from Banking4 for a real
vault were compared with this file, for bookings, payments and standing
orders. Every name in it belongs to a field of this file: 48 names for
bookings, 26 for payments, 31 for standing orders, plus two that belong to
the export itself. No name was left unexplained. Fields of this file that
the export does not show are memory-only, or are left out by the export
when they are empty. That says nothing about values, and nothing about
single-chunk records. For both see 13.6.

### 13.5 Not confirmed

Not confirmed:

- that `Acct<oid>/StmtMasterEntries` is merged into the bookings and then
  deleted (section 1), and the end of a booking file load as a whole (the
  step that sorts, regroups and merges the legacy file): the lists of the
  fixtures are those of one record file on its own. For that step inside a
  vault there is one confirmation, on a test file produced with Banking4: in
  the demo vault (`tests/fixtures/vault/`), whose statement files hold one
  correctly written split and one correctly written batch, Banking4's sort
  and regrouping of the bookings drops nothing, changes nothing and leaves
  every file in its order;
- editions. The expected results of the fixtures are those of Banking4's
  default product variant. That variant is not the full edition, and
  batches, among other things, are only loaded when the professional or the
  business edition is selected (document 03, "What is loaded when"). How
  that relates to the batch groups in the sort and regrouping rows of 13.2,
  which are those of the default variant and of a record file on its own, is
  not confirmed. The expected results of the demo vault are those of the
  professional edition;
- that the split editing row is never loaded from or saved to a vault file;
- that the user's category assignment goes through the category catalogue
  (5.7);
- what the shipped manifest contains for the default currency (6.2), and
  where the regional settings behind that currency come from: this may
  differ between the platforms Banking4 runs on and is not confirmed for all
  of them;
- cross entries (6.5), the displayed transaction code (6.6), balances (6.7;
  for the last see the one confirmation on the demo vault there);
- the outbox rule that a repeated `order_reference` is dropped from the later
  order (10.1 position 13), and the parsing of the outbox order's JSON text
  (10.1 position 32);
- the standing order schedule (9.1 position 6 and 9.2), and with it the sort
  position of standing orders that are not local: the fixtures only contain
  such standing orders with no further execution or with executions without
  end;
- strings in the long length form inside these records (the primitive is
  covered by document 02);
- how Banking4 reports a rejected record to the user. That the load of a file
  stops at such a record is taken from document 02.

This file describes one Banking4 version. The details document 02 lists as
decided by the runtime Banking4 runs on (02, open question 8) apply here as
well: they may differ between the platforms Banking4 runs on and are not
confirmed for all of them.

### 13.6 Compared with a real vault

The output of `b4`'s decoders was compared, for one real vault written by a
newer program version than the Banking4 version described here, value by
value with data exported from Banking4 for the same vault. No differences
were found:

| Record | Records compared |
|---|---|
| booking | 21,931 |
| standing order | 25 |
| payment | 3 |

The database `b4` writes from that vault was compared with the exported data
as well; for accounts and bookings together that is 1.34 million values, all
equal.

What that vault contains, and does not:

- **Legacy single-chunk records occur.** 837 bookings, the whole statement
  file of one account, and all 3 payments are in the single-chunk layout of
  5.2. None of them has a tagged purpose line, so the comparison confirms
  the layout and the plain purpose text of 5.8 on real data, not the tags;
  those rest on the fixtures.
- **No splits, no batches, no hidden payments.** The regrouping of 6.4, the
  rule for what counts (6.4), the collection adjustments of 8.3 for groups
  and the payment state `0x80` are not confirmed by it.
- No standing order whose comparison depends on the schedule: the export
  shows stored values.

It is one vault of one program version. The exported data holds the values
Banking4 works with after loading, so fields that Banking4 discards while
loading or that the export leaves out were not compared, and the TAN fields
of the payment are dropped by `b4` at decode time and were never compared
with real values.

## 14. Open questions

1. **Do single-chunk records still occur?** Answered: yes, and a reader
   must decode them. One real vault holds 837 bookings (one whole account
   file) and 3 payments (all it has) in the single-chunk layout of 5.2, and
   decoded by 5.8 they show no difference from the data exported from
   Banking4 (13.6). None of them carries a tagged purpose line. Still open:
   which program versions wrote such records, and whether real records with
   tagged lines exist; the tags are covered by the fixtures only. A reader
   counts single-chunk records per file and reports the count.
2. **Language of the pseudo category names (5.7).** The comparison uses the
   language Banking4 is running in. Where a vault records that language, if it
   does at all, is not known from these records. Treating the names of all
   three shipped languages as pseudo categories is a safe superset for step 4;
   step 3 needs a choice.
3. **Application-level text overrides.** Banking4 looks a text up among the
   texts of the application build first and among the built-in ones second.
   The standard build carries none of its own for the category names
   (not confirmed); a rebranded build could.
4. **Displayed transaction code (6.6).** The mapping tables from purpose code,
   business transaction code and PayPal transaction text to a transaction code
   are not given here.
5. **Cross entries (6.5).** The text comparison that decides whether two
   candidate bookings match is not specified.
6. **JSON texts.** The payment's text is described in 8.5; parsing and
   re-serialising it is not lossless (unknown members and empty members go,
   the spacing changes). Still open: the structure of the outbox order's text
   (10.1 position 32), the exact type conversions of the JSON parser, and
   whether the amount keeps its digits in every case when written again (in
   the fixtures `"1.50"`, `"7.5"` and `12.5` do).
7. **Sign of `instructed_amount` and `charges_amount`.** They are stored as
   delivered by the statement import; whether they can be negative is not
   known. `vat_amount` is used as an unsigned part of `amount`.
8. **Can `amount` be negative in practice?** The application refuses to set a
   negative amount, the reader accepts one. Whether old versions wrote any is
   unknown.
9. **Naming of `debit_return_*`.** The three fields are used when a direct
   debit is returned online and are filled from the bank's statement data; the
   names chosen here reflect that use and are an interpretation.
10. **Meaning of some codes**: the values of `intl_remittance_method`, of
    `regulatory_reporting_code` and of `gvc_extension`, and what service
    `entry_bits` `0x04` refers to, are not known.
11. **Account kind combinations.** The rules in 6.2 and 6.6 compare
    `account_kind` for equality with single values. Document 03 states that a
    stored account has exactly one kind bit; if that ever fails, these rules
    silently do not apply.
12. **Other program versions and platforms.** This file describes one
    Banking4 version. Whether every platform Banking4 runs on behaves the
    same is not confirmed (13.5). Older and newer versions may write
    fewer or more trailing fields per chunk; the short-chunk defaults of
    section 2 exist for exactly that, but which fields were added when is
    unknown. For one vault written by a newer program version, `b4`'s output
    shows no difference from the data exported from Banking4, for all three
    record types (13.6).
13. **Signature folder items** share the payment base. Their layout is in
    document 05, section 3.13; this entry is kept only so that the numbering
    of the list stays stable. What is still open about them is listed there
    (05, open question 5).
14. **The default currency of the shipped product** (6.2). Banking4 reads
    the manifest parameter when it runs; no such parameter was found in the
    plain files of an installation, so it may be absent or embedded. Without
    it the default follows the regional settings (EUR or CHF) and is EUR
    otherwise.
15. **The standing order schedule.** The rule that computes the next
    execution from first date, period, day and last date is not given here.
    Without it the next date Banking4 shows for a standing order that is not
    local, and its position in Banking4's list, cannot be reproduced.
16. **Split editing row and outbox order.** The repository has no fixtures
    for them, so the layouts of sections 7 and 10 cannot be checked with it.
17. **Do pending bookings ever lack a date in real vaults, and do payments
    without a currency occur?** The rules for both are covered by the
    fixtures (6.2, 6.4, 5.11); how common the cases are is not known, and
    the comparison of 13.6 recorded no count of either.
