# 02 - Record wire format

This document describes how Banking4 serialises its data model into the files
stored inside a vault: how a collection file is cut into records, how a record
is cut into chunks, and how each primitive value is laid out inside a chunk.
It is written to be sufficient for implementing a reader.

It does **not** describe which fields each record type contains; those are in
the per-record documents. It also does not describe the encrypted container
that holds the files (see document 01).

## 0. Status, conventions, terminology

**Status.** This document describes the format as one build of Banking4
reads and writes it. The test files for it are in `tests/fixtures/wire/`.
Section 10 says what they are and what they cover, what was corrected in
this document, and what is not confirmed. All byte examples below are
synthetic values; most were produced with Banking4. Statements that are not
confirmed are marked *(not confirmed)*.

**Conventions.**

- All multi-byte integers are **little-endian**.
- Hex bytes are written `E8 07 02 1D`, in file order.
- Names such as `record_size`, `chunk_end` or `u16` are this project's own
  names.
- "The reader" / "the writer" mean Banking4 reading and writing these files.
  Rules that are *our* policy for `b4` are in section 8.
- "The Banking4 version described here" means the one build of Banking4 that
  these documents describe: the build of one Windows installation (program
  files dated 2026-05-18; it writes account records of version 26), the same in
  documents 01 to 05 and in the README of this directory. Other versions may
  differ (open question 3).

**Terminology.**

| Term | Meaning |
| --- | --- |
| collection file | one file inside the vault holding all records of one collection (accounts, bookings of one account, payees, ...) |
| record | the serialised form of one object |
| chunk | a length-prefixed slice of a record; a record is one or more chunks |
| `cursor` | position of the next byte to read |
| `chunk_end` | first position after the current chunk |
| `record_end` | first position after the record |

## 1. Collection files

### 1.1 Binary or XML

Whether a file is binary or XML is **not** detected from the file. There is no
magic number, no version field and no file header. The format is a fixed
property of the kind of file, decided by Banking4 and not stored in the file
*(not confirmed)*:

- Every collection (about 30 kinds in the Banking4 version described here:
  accounts, account groups, bookings, payments, standing orders, portfolio
  items, payees, payers, categories, exchange rates, stock instruments, stock
  prices, settings, and so on) uses the **binary** format described here.
- Exactly one kind of file uses **XML**: a data file belonging to certain
  kinds of bank access contacts. `b4` never reads those files, so the XML form
  is not described.

A reader therefore decides from the *file name / location* how to decode a
file, never from its content. The file map is in
[03-model-and-accounts.md](03-model-and-accounts.md), Part 1; it also says
which document holds the layout of each record type.

Other files in a vault that are not collections at all (stored attachments,
avatar images, order data of the outbox, bank access contacts) are handled
differently and are raw payloads or XML; they are outside this document.

### 1.2 Layout of a binary collection file

A binary collection file is nothing but records, back to back:

```
file    := record*
record  := record_size:i32  body[record_size]
body    := chunk+  slack?
```

- `record_size` counts the bytes of `body` only. It does **not** include its
  own four bytes.
- There is no record count, no terminator and no padding between records.
- An empty file (zero bytes) is a valid, empty collection.

Reader algorithm:

1. Try to read 4 bytes. If fewer than 4 bytes are left, the file ends here.
   This is silent: 1 to 3 stray bytes at the end of a file are ignored.
   (The container's file stream only returns a short read at the true end of
   the file, so this is a genuine end-of-file test.)
2. Interpret them as signed `record_size`. It must satisfy
   `4 <= record_size <= 0x4000_0000`; anything else (including zero and
   negative values) is a corrupt file. The upper bound is inclusive: a record
   of exactly `0x4000_0000` bytes is accepted. The bound is tested before any
   attempt to read the body.
3. Read exactly `record_size` bytes. Hitting the end of the file first is a
   corrupt (truncated) file.
4. Decode the record from those bytes (section 2). Go to 1.

Banking4 reads each record body into a buffer of its own, so inside a record
every position (cursor, chunk bounds, `record_end`) counts from the first byte
of the body.

After the record has been decoded Banking4 performs one more check: if the
chunk the decoder stopped in still has its *more chunks* flag set, the whole
file is rejected with a **compatibility** error (meaning "written by a newer
program version"). See 2.4.

Banking4 stops loading a file at the first error; it does not resynchronise.
A file that does not exist is simply an empty collection, not an error
*(not confirmed)*.

Synthetic example, a file with two records:

```
05 00 00 00                      record_size = 5
   05 00 00 00  10               one chunk, size 5, payload 10
0B 00 00 00                      record_size = 11
   05 00 00 80  12               chunk, size 5, more chunks follow, payload 12
   06 00 00 00  02 01            chunk, size 6, last, payload 02 01
```

### 1.3 Writer

For every object the writer serialises the record into a buffer, then emits
`record_size` followed by the buffer. `record_size` is the sum of the sizes of
all chunks written, so the writer never produces slack. An object may decline
to be written, in which case nothing at all is emitted for it.

### 1.4 Records outside collection files

A record can also appear without the `record_size` prefix: at least one record
type (saved statement filters) is additionally stored as Base64 text whose
decoded bytes are the bare chunk sequence. There the length of the decoded
buffer plays the role of `record_size`, and no end-of-record flag check is
made *(not confirmed)*.

## 2. Records and chunks

### 2.1 Chunk layout

```
chunk        := chunk_header:u32  payload[chunk_size - 4]
chunk_header := bit 31      more_chunks   1 = another chunk follows in this record
                bit 30      ignored       masked off by the reader, never set by the writer
                bits 0..29  chunk_size    includes the 4 header bytes
```

- `chunk_size = chunk_header & 0x3FFF_FFFF`, `more_chunks = chunk_header &
  0x8000_0000 != 0`.
- `chunk_size` **includes** its own 4-byte header (unlike `record_size`), so
  the smallest legal value is 4, an empty chunk.
- The largest `chunk_size` is `0x3FFF_FFFF`. Because bit 30 is masked off, a
  header such as `00 00 00 40` does not mean "a chunk of `0x4000_0000` bytes"
  but a chunk of size 0, which is corrupt. A record of the maximum size
  (`0x4000_0000`) therefore cannot be a single chunk.
- The next chunk, if any, starts exactly at `chunk_start + chunk_size`.

### 2.2 Reader state

| Name | Meaning |
| --- | --- |
| `record_start`, `record_end` | bounds of the record body |
| `cursor` | next byte to read |
| `chunk_start` | position of the current chunk's header |
| `chunk_end` | `chunk_start + chunk_size` |
| `more_chunks` | flag of the **current** chunk |
| `consumed_size` | sum of `chunk_size` of all chunks opened so far |

### 2.3 Operations

**Open a chunk** (at `cursor`):

1. If `cursor + 4 > record_end`: corrupt.
2. Read the header. If `chunk_size < 4` or `cursor + chunk_size > record_end`:
   corrupt.
3. `chunk_start = cursor`, `chunk_end = cursor + chunk_size`,
   `more_chunks` = bit 31 of the header, `consumed_size += chunk_size`,
   `cursor += 4`.

**Open a record:** `cursor = record_start`, `consumed_size = 0`, then open a
chunk. Every record therefore has at least one chunk.

**Advance to the next chunk:**

- If `more_chunks` is clear: report "no further chunk" and change **nothing**.
  The cursor stays where it is, inside the current chunk, and later reads carry
  on from there (and return defaults once `chunk_end` is reached).
- Otherwise: `cursor = chunk_end` (unread bytes of the current chunk are
  skipped), then open a chunk. A set flag with no valid chunk behind it is
  corrupt. The cursor is repositioned from `chunk_end` wherever it was before,
  including when an over-long string has moved it to `record_end` (section 3).

**Close a record:** nothing is checked. In particular it is not an error if
the cursor is short of `chunk_end`, nor if `consumed_size < record_size`
(slack after the last chunk is never looked at).

All primitive reads are bounded by `chunk_end`, never by `record_end`: a value
never straddles two chunks, and a read can never see bytes of the next chunk.

### 2.4 What chunks are for

Chunks are how a record is layered and versioned:

- **Layering.** A record type that derives from another writes the base type's
  fields first. Some derived types then start a new chunk before their own
  fields. In the Banking4 version described here exactly five record types do
  this (accounts, and four types built on the common payment base: payments,
  bookings, standing orders, and signature folder items, which 03 calls pay
  checks after their file name), each exactly once, so their records have two
  chunks. No other record type of that version changes chunks, neither when it
  is read nor when it is written, so every other record type is single-chunk.
- **Appending fields.** A newer program version adds fields at the *end* of a
  chunk. An older reader stops early and ignores the rest of that chunk
  (2.3: advancing skips unread bytes; closing checks nothing). A newer reader
  given an old, shorter chunk gets default values for the missing fields
  (section 3).
- **Version probing.** A record decoder may ask whether the current chunk has
  `more_chunks` set and choose a legacy interpretation if not. The common
  payment base does this.

What chunks do **not** give is forward compatibility for *new chunks*. The
record types that advance do so unconditionally and ignore the result, and
after a record has been decoded, a still-set `more_chunks` flag makes Banking4
reject the entire file as incompatible (1.2). So:

| Situation in the file | Banking4 |
| --- | --- |
| extra bytes at the end of a chunk | ignored |
| chunk shorter than the fields expected | missing fields read as defaults |
| fewer chunks than the decoder advances through | advance is a no-op, reading continues in place |
| more chunks than the decoder advances through | file rejected (compatibility error) |
| slack between last chunk and `record_end` | ignored |

### 2.5 Writer

The writer reserves 4 bytes for the header, writes the payload, and patches the
header when the chunk is closed: `chunk_size`, plus bit 31 if the chunk was
closed by starting another one. The final chunk is closed without the flag.
An object with no fields at all would be written as `04 00 00 00`.

Synthetic examples (produced with Banking4):

```
05 00 00 00  2A                         one chunk, payload 2A
05 00 00 80  01   05 00 00 00  02       two chunks, payloads 01 and 02
```

## 3. Reading past the end of a chunk

This is the compatibility mechanism, so it is specified exactly. Let
`remaining = chunk_end - cursor`. This difference can be **negative**: after
an over-long string the cursor lies beyond `chunk_end` (step 4 below). It has
to be computed in signed arithmetic, or clamped at zero, never as an unsigned
subtraction.

**General rule for fixed-size primitives:** if `remaining` is smaller than the
size of the value, the read returns the type's default and the cursor **does
not move**. Whatever bytes were left stay unread (and a later, smaller read
can still consume them). Nothing is logged and nothing fails. An exact fit
succeeds.

| Primitive | Needs | Result when `remaining` is too small | Cursor afterwards |
| --- | --- | --- | --- |
| `u8` | 1 | `0` | unchanged |
| `bool` | 1 | `false` | unchanged |
| `tristate` | 1 | unknown (null) | unchanged |
| `u16` | 2 | `0` | unchanged |
| `i32` | 4 | `0` | unchanged |
| `i64` | 8 | `0` | unchanged |
| `decimal` | 16 | `0` | unchanged |
| `guid` | 16 | all-zero GUID | unchanged |
| `code(n)` | n | null | unchanged (`code(0)` never reads anything and is always null) |
| `date_raw` | 4 | null date | unchanged |
| `date_clamped` | 4 | no date | unchanged |
| `ticks` | 8 | no timestamp | unchanged |
| `time_hms` | 3 | null time | unchanged |
| `date_strict` | 2+1+1 | no date | **advances over whatever sub-reads fit** (see below) |
| `string` | 2 (+4) + len | null | see below |
| `blob` | 2 (+4) + len | null, or fatal | see below |

**`date_strict`** is not one 4-byte read but three independent reads
(`u16`, `u8`, `u8`), each following the general rule on its own:

| `remaining` | Sub-reads that succeed | Cursor advance | Result |
| --- | --- | --- | --- |
| 0 | none | 0 | no date |
| 1 | the first `u8` (the lone byte is taken as the month) | 1 | no date |
| 2 | `u16` only | 2 | no date |
| 3 | `u16` and the first `u8` | 3 | no date |

**`string`.** The reader first reads the length prefix (4.8), then decides in
this order:

| Step | Condition | Result | Cursor afterwards |
| --- | --- | --- | --- |
| 1 | `remaining < 2` (length prefix missing) | null | unchanged |
| 2 | prefix is `FF FF` but fewer than 4 bytes follow | null | advanced by 2 (the marker is consumed, the incomplete length is not) |
| 3 | length is 0 | null | just after the length prefix |
| 4 | `cursor + length > chunk_end` | null, a message is logged | **set to `record_end`** |
| 5 | length is negative (only possible in the long form) | null, a message is logged | moved **backwards**: `cursor + length`, counted from just after the length prefix (a defect of Banking4; treat as corrupt) |
| 6 | otherwise | the decoded string | after the string bytes |

In step 2 the unread bytes of the incomplete 32-bit length stay in the chunk and
are consumed by later, smaller reads.

Step 4 is the special path: the cursor jumps to the end of the whole record,
not to the end of the chunk. Whenever another chunk or slack follows the
current chunk, `record_end` lies beyond `chunk_end`, so **the cursor is then
outside the chunk it is reading**. Every later read in that chunk returns its
default and leaves the cursor where it is, because `remaining` is zero or
negative; the chunk is "poisoned". The record is not lost entirely, because
advancing to the next chunk repositions the cursor from `chunk_end` and
decoding resumes normally there. If the chunk was the last one the rest of
the record reads as defaults.

A reader has to get this right in its own arithmetic. With an unsigned
`chunk_end - cursor` the subtraction either aborts, so that one crafted
string length takes down the whole file, or wraps to a huge number, so that
the next field is read at `record_end` from the bytes of the following
record or of slack and every later field of the chunk is silently wrong.
`b4` treats `remaining` as zero whenever the cursor is at or beyond
`chunk_end` (section 8, rule 7).

Synthetic example, two chunks; the first holds a string claiming 5 bytes with
only 3 left:

```
09 00 00 80  05 00 41 42 5A      chunk 1: size 9, more chunks follow
0C 00 00 00  EE EE EE EE EE EE EE EE   chunk 2: size 12
```

Reading `string`, `u8`, then advancing and reading `u8` gives: null with the
cursor at 21 (`record_end`); `0` with the cursor still at 21; the advance
succeeds and puts the cursor at 13; `0xEE` with the cursor at 14.

**`blob`** shares steps 1 to 3 with `string` (a length of zero or *below*
yields null with the cursor just after the length prefix), but an over-long
length (step 4) is **fatal**: a corrupt-stream error that aborts the file load.

**Absurd lengths wrap around.** The test of step 4 is made in signed 32-bit
arithmetic that wraps. A long-form length of `0x8000_0000 - cursor` or more
(nearly 2 GiB; a record is at most 1 GiB, so only a damaged length field can
say this) makes `cursor + length` negative and so *passes* the test:

- `string`: nothing is decoded, a message is logged and null is returned, as in
  step 5, and the cursor is advanced by the length with wrap-around, which
  leaves it far below zero. Any later read of one or more bytes in that chunk
  fails; zero-width reads still return null, and advancing to a next chunk, if
  there is one, repositions the cursor as usual.
- `blob`: the read fails, but with an ordinary runtime error rather than the
  corrupt-stream error.

`b4` must not reproduce this: compare `length` with `remaining` instead of
computing the sum, and treat such a length as over-long (section 8).

## 4. Primitives

For each primitive: size on the wire, layout, value mapping, the
null/default representation, the writer's behaviour, and an example. End of
chunk behaviour is in section 3 and only repeated where it is special.

### 4.1 `u8`

- **Size:** 1.
- **Value:** unsigned 0..255. Used for small enumerations and flag sets.
- **Default:** `0`.
- **Example:** `AB` → 171.

### 4.2 `bool`

- **Size:** 1.
- **Value:** `00` → false, any other value → true. The writer emits `00` or
  `01`.
- **Default:** false.
- **Example:** `01` → true; `FF` → true.

### 4.3 `tristate`

- **Size:** 1.
- **Value:** `00` → false, `01` → true, anything else → unknown (null). The
  writer emits `02` for unknown.
- **Default:** unknown. Note that this differs from `bool`: a missing
  `tristate` is "unknown", not "false".
- **Example:** `02` → unknown; `07` → unknown.
- **Use:** none. The format has the primitive, but no record type of the
  Banking4 version described here uses it, so no table in documents 03 to 05
  contains a `tristate`. It is specified for completeness; the test files of
  section 10 cover it.

### 4.4 `u16`

- **Size:** 2.
- **Layout:** low byte, high byte.
- **Value:** the reader returns the **unsigned** value 0..65535 (as a wider
  integer). It never returns a negative number.
- **Writer:** takes a 32-bit integer and stores its low 16 bits, without range
  checking. So `-1` is written as `FF FF` and read back as 65535, and 70000 is
  written as `70 11` and read back as 4464.
- **Signedness is a per-field matter.** Most fields use the unsigned value
  directly (counts, enumerations, flag sets). For exactly two record fields of
  the Banking4 version described here the value is reinterpreted as signed
  16-bit after reading: the booking's `gvc` and `gvc_extension` (04, section
  6.1). Every other `u16` field of documents 03 to 05 is unsigned.
- **Default:** `0`.
- **Example:** `34 12` → 4660 (0x1234); `00 80` → 32768.

The value 65535 has a special meaning only inside `string` and `blob` length
prefixes (4.8, 4.10), not for `u16` fields in general.

### 4.5 `i32`

- **Size:** 4.
- **Value:** signed two's complement.
- **Default:** `0`.
- **Example:** `78 56 34 12` → 305419896; `FE FF FF FF` → -2.

Besides plain numbers, `i32` carries object ids and update stamps (5.1) and
references to other objects by id.

### 4.6 `i64`

- **Size:** 8.
- **Value:** signed two's complement.
- **Default:** `0`.
- **Example:** `08 07 06 05 04 03 02 01` → 0x0102030405060708.

(Banking4 uses the platform's native byte order for this type and for `ticks`;
on every platform the program runs on that is little-endian.)

As a field type of its own `i64` is not used by any record of the Banking4
version described here; it only occurs as the carrier of `ticks` (4.13).

### 4.7 `decimal`

- **Size:** 16.
- **Layout:** four 32-bit words: `lo`, `mid`, `hi`, `flags`.

  | Offset | Field | Meaning |
  | --- | --- | --- |
  | 0 | `lo` | bits 0..31 of the 96-bit unsigned magnitude |
  | 4 | `mid` | bits 32..63 |
  | 8 | `hi` | bits 64..95 |
  | 12 | `flags` | bits 16..23 = `scale` (0..28); bit 31 = sign (1 = negative); all other bits zero |

- **Value:** `(-1)^sign × (hi·2^64 + mid·2^32 + lo) × 10^(-scale)`. The three
  magnitude words are unsigned even though they are stored as 32-bit signed
  integers.
- This is exactly the .NET decimal bit representation, and the writer stores it
  bit for bit. Consequences: the **scale is preserved** (`1.10` and `1.1` are
  different byte strings and read back with their trailing zeros intact), and a
  negative zero can occur (it compares equal to zero).
- **Default:** `0` (16 zero bytes).
- **Invalid encodings:** a `scale` above 28 or any non-zero reserved bit in
  `flags` (bits 0..15 and 24..30) makes Banking4 fail (the file load is
  aborted). The magnitude is never invalid: all 96 bits set with scale 28 is
  a legal value.
- **No legacy variant exists.** There is one layout only.
- **Examples:**

  | Bytes | Value |
  | --- | --- |
  | `D2 04 00 00  00 00 00 00  00 00 00 00  00 00 02 00` | 12.34 |
  | `05 00 00 00  00 00 00 00  00 00 00 00  00 00 02 80` | -0.05 |
  | `6E 00 00 00  00 00 00 00  00 00 00 00  00 00 02 00` | 1.10 |
  | `00 00 00 00  01 00 00 00  00 00 00 00  00 00 00 00` | 4294967296 |
  | `01 00 00 00  00 00 00 00  00 00 00 00  00 00 1C 00` | 0.0000000000000000000000000001 |

  In Rust, `rust_decimal::Decimal::from_parts(lo, mid, hi, negative, scale)`
  takes these fields directly.

**Non-negative decimals.** For some decimals of one record type (portfolio
items) Banking4 replaces a negative result by zero when it reads them. This
is *not* a different wire layout: the bytes are an ordinary `decimal`, the
clamping happens after decoding, and the writer does not clamp. The per-record
document marks the affected fields.

### 4.8 `string`

- **Size:** 2 + length, or 6 + length in the long form.
- **Layout:**

  ```
  short form:  length:u16  bytes[length]              length 0..65534
  long form:   FF FF  length:i32  bytes[length]       used when length >= 65535
  ```

  The length counts **bytes**, not characters.
- **Encoding:** UTF-8, no byte order mark, no terminator. The encoding is
  plain UTF-8 for every record string; no field and no setting changes it.
  Banking4 does use Windows-1252 elsewhere (for a built-in category catalogue
  and a text import/export format), but record strings are never in a Windows
  code page. German umlauts are therefore two bytes each (`ü` = `C3 BC`,
  `ß` = `C3 9F`), `€` is three.
  - The decoder is lenient: an invalid byte sequence becomes U+FFFD rather than
    an error. *How many* U+FFFD a malformed sequence produces is decided by
    the UTF-8 decoder of the runtime Banking4 runs on, not by Banking4
    itself, and decoders differ: for `E0 80 80` Banking4 can return two,
    where a decoder following the Unicode "maximal subpart" practice (Rust's
    `String::from_utf8_lossy`, for one) returns three. The number may differ
    between the platforms Banking4 runs on and is not confirmed for all of
    them (open question 8). A single stray byte (`FF`, a lone `C3`) is one
    U+FFFD everywhere. Only malformed data is affected, which the writer
    never produces.
  - On the writing side each unpaired UTF-16 surrogate of the in-memory string
    is stored as U+FFFD (`EF BF BD`).
  - Nothing is stripped or normalised: a leading `EF BB BF` yields U+FEFF as
    the first character; embedded NUL, CR and LF are kept.
- **Null and empty:** a length of 0 means **null**. The writer stores both a
  null string and an empty string as `00 00`, so the two cannot be told apart
  and an empty string comes back as null.
- **Long form:** the writer switches to it when the byte length is 65535 or
  more (65534 bytes is the largest short-form string). It is the UTF-8 byte
  length that decides, not the number of characters: 32767 times `ü` is 65534
  bytes and short form, one more ASCII character makes it long form. The
  reader accepts the long form for any length, including small ones and zero
  (`FF FF 00 00 00 00` is null).
- **Default:** null.
- **End of chunk:** see section 3 (six steps, one of which jumps the cursor to
  `record_end`).
- **Examples:**

  | Bytes | Value |
  | --- | --- |
  | `00 00` | null |
  | `07 00  47 72 C3 BC C3 9F 65` | `Grüße` (5 characters, 7 bytes) |
  | `03 00  E2 82 AC` | `€` |
  | `FF FF  02 00 00 00  41 42` | `AB` (long form, accepted) |
  | `FF FF  FF FF 00 00  61 ...` | a 65535-byte string |
  | `02 00  C3 28` | U+FFFD followed by `(` |

### 4.9 `code(n)`

A fixed-width field for short codes whose maximum length is known: currency
codes, country codes, and the like. `n` is not on the wire; it is part of the
record definition.

- **Size:** exactly `n` bytes.
- **Layout:** the characters of the code, one byte each, padded on the right
  with `00` up to `n`.
- **Encoding:** each byte is one character with the same code point (i.e.
  ISO 8859-1). This is *not* UTF-8. In practice the content is ASCII.
- **Reader:** takes the `n` bytes as `n` characters, removes trailing NUL
  characters, then removes leading and trailing white space. If nothing is
  left the result is **null**.
  - White space is what the string trimming of the runtime Banking4 runs on
    considers white space. Among the 256 possible bytes these are exactly
    `09`..`0D`, `20`, `85` and `A0` (the reader cases `crafted/code_trim/00`
    to `crafted/code_trim/e0` in `tests/fixtures/wire/reader-cases.tsv` hold
    all 256 values; this is also the Unicode `White_Space` set in that
    range, so Rust's `char::is_whitespace` agrees). `1C`..`1F` are *not*
    trimmed. The set may differ between the platforms Banking4 runs on and
    is not confirmed for all of them (open question 8).
  - Each of the two steps happens once, in that order. NULs that are not at
    the end are kept, and so is a NUL uncovered by the white space trimming:
    `41 00 20` gives `A` followed by a NUL, and a leading NUL shields what
    follows it (`00 20 41` gives NUL, space, `A`).
- **Writer:** writes the low byte of each of the first `n` characters, then
  NUL padding. A longer code is silently cut to `n`; a character above U+00FF
  is silently mangled to its low byte; null writes `n` NUL bytes.
- **Default:** null. `code(0)` is always null.
- **Examples:**

  | Width | Bytes | Value |
  | --- | --- | --- |
  | 3 | `45 55 52` | `EUR` |
  | 3 | `44 45 00` | `DE` |
  | 2 | `00 00` | null |
  | 3 | `20 20 20` | null |
  | 3 | `20 41 20` | `A` |
  | 3 | `41 00 42` | `A`, NUL, `B` |
  | 3 | `41 00 20` | `A`, NUL |
  | 2 | `41 A0` | `A` |
  | 2 | `C4 D6` | `ÄÖ` |

### 4.10 `blob`

- **Size:** 2 + length, or 6 + length.
- **Layout:** the same length prefix as `string` (short form up to 65534,
  `FF FF` + `i32` from 65535 bytes), followed by the raw bytes.
- **Null and empty:** a length of 0 (or below) means **null**. The writer
  stores a null and an empty byte array identically as `00 00`.
- **Default:** null.
- **End of chunk:** differs from `string`: a length that exceeds the chunk is a
  fatal corrupt-stream error (section 3).
- **Example:** `04 00  DE AD BE EF` → 4 bytes `DE AD BE EF`.
- **Use:** two fields of the records `b4` reads, both in document 05: the
  content digest of an account document and the order data of a signature
  folder item.

### 4.11 `guid`

- **Size:** 16.
- **Layout:** the .NET byte layout of a GUID: the first three groups are
  little-endian, the last two are in display order.

  | Offset | Size | Field |
  | --- | --- | --- |
  | 0 | 4 | group 1, little-endian |
  | 4 | 2 | group 2, little-endian |
  | 6 | 2 | group 3, little-endian |
  | 8 | 8 | groups 4 and 5, as displayed |

- **Default:** the all-zero GUID, which is also how "no GUID" is written.
- **Example:** `33 22 11 00  55 44  77 66  88 99 AA BB CC DD EE FF` →
  `00112233-4455-6677-8899-aabbccddeeff`. In Rust:
  `uuid::Uuid::from_bytes_le`.

### 4.12 Calendar dates: one layout, three readers

All calendar dates share one 4-byte layout:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 2 | `year`, `u16` |
| 2 | 1 | `month` |
| 3 | 1 | `day` |

"No date" is `00 00 00 00`. There is no time of day and no time zone; these are
plain civil dates (booking date, value date, ...).

Two writers produce these bytes:

- The **raw date writer** (fields read with `date_raw`) stores the three
  components exactly as they are held in memory: partial and impossible
  values included, nothing validated.
- The **timestamp writer** (fields read with `date_clamped` or `date_strict`)
  takes a timestamp. The zero timestamp (0001-01-01T00:00:00, tick count 0,
  whatever its UTC/local marker) is written as `00 00 00 00`. Any other
  timestamp is written as its calendar date; the time of day is discarded.

In memory the two timestamp readers represent "no date" *as* the zero
timestamp, which is also midnight of 0001-01-01. So for them `00 00 00 00` and
`01 00 01 01` mean the same thing, and the writer does produce the latter: a
timestamp on 0001-01-01 with a non-zero time of day is written as
`01 00 01 01` and reads back as "no date".

The three readers differ in validation and in end-of-chunk behaviour, and each
record field uses one particular reader, so the per-record documents name the
variant. In the Banking4 version described here the distribution is:

| Reader | Fields |
| --- | --- |
| `date_strict` | one: the account's `sevdesk_last_entry_date` (03) |
| `date_clamped` | seven: five dates of the account (03) and the booking's `analysis_date` and `transaction_date` (04) |
| `date_raw` | every other calendar date in documents 04 and 05 |

#### `date_raw`

The reader takes the four bytes as they are and keeps them in a packed integer
`year << 16 | month << 8 | day`. **Nothing is validated.**

- **Null:** all four bytes zero. A missing field (short chunk) is also null.
- **Partial dates are representable and are not null:** `E8 07 00 00` is "year
  2024, month 0, day 0". When Banking4 builds such a date from a year, a
  month and a day in memory it refuses this value (it accepts either
  all-zero, or year 1..9999, month 1..12, day 1..31 with no per-month check,
  so 31 February is accepted), but the reader does not apply that check.
  Changing a single component of a date in memory validates only that
  component (1..9999, 1..12, 1..31), so setting one component of a null date
  leaves a partial value such as "year 2024, month 0, day 0". Whether any
  record actually stores partial dates is a per-record question.
- **Conversion to a real date,** when the program needs one: null → no date;
  exactly 9999-12-31 → "end of time" (the maximum timestamp,
  9999-12-31T23:59:59.9999999); otherwise year is clamped to 1..9999, month to
  1..12, day to 1..(days in that month), giving midnight of that day. So a
  year-only value becomes 1 January and 31 February becomes the last day of
  February. A value that clamps to 0001-01-01 (`01 00 01 01`, but also
  `00 00 01 01` or `00 00 00 01`, which are not null) converts to the zero
  timestamp and is thus "no date" after conversion.
- **A year with the high bit set.** The packed integer is signed, so a high
  byte of `year` at or above `80` makes it, and thus the year the reader
  returns, negative: `FF FF 01 01` is year -1, month 1, day 1, and
  `00 80 01 01` is year -32768. Banking4 does not fail on this and reads the
  following field normally. The conversion then clamps the year to 1 like
  any other out-of-range part: `FF FF 01 01` and `00 80 01 01` clamp to
  0001-01-01 and so convert to "no date", while `FF FF FF FF` (year -1,
  month 255, day 255) converts to 0001-12-31, which looks like a real date.
  (Reader cases `crafted/date/ffff0101`, `00800101` and `ffffffff` in
  `tests/fixtures/wire/reader-cases.tsv`.) Banking4 cannot write such bytes.
- **Examples:** `E8 07 02 1D` → 2024-02-29; `00 00 00 00` → null;
  `0F 27 0C 1F` → 9999-12-31.

**What `b4` does with a `date_raw`.** There is one policy, in two layers:

1. The wire layer reproduces Banking4's reading exactly: it returns the three
   components as stored, a negative year included, never fails on them, and
   offers the clamping conversion above. Nothing is "corrupt" at this layer.
2. The database stores a date only when the stored year, month and day are a
   real calendar date of the years 1 to 9999. 0001-01-01 is "no date", as in
   Banking4, and is stored as NULL. Every other value (a partial date, month
   13, 31 February, a negative year) is also stored as NULL, and a warning
   names the table and the column and gives the number of such values, never
   the values themselves. The clamped date is not stored: it would present
   `FF FF FF FF` as 31 December of the year 1 and a year-only value as
   1 January.

**The record is never dropped** because of a date: its other fields, the
amount among them, are kept.

#### `date_clamped`

Same bytes, decoded directly to a timestamp at midnight:

- If `year`, `month` **or** `day` is zero → no date. (So a partial date is
  "no date" here, whereas `date_raw` plus conversion would give 1 January.)
- Otherwise `year` is capped at 9999, `month` at 12, `day` at the number of
  days in that month. `year` is unsigned here: `FF FF 01 01` is 9999-01-01,
  where `date_raw` sees the year -1.
- 9999-12-31 is returned as that plain date (midnight), not as "end of time".
- 0001-01-01 is returned as the zero timestamp, i.e. as "no date" (see above).
- **Examples:** `E8 07 02 1F` → 2024-02-29; `E8 07 0D 05` → 2024-12-05;
  `E8 07 00 00` → no date; `10 27 01 01` (year 10000) → 9999-01-01;
  `01 00 01 01` → no date.

#### `date_strict`

Same bytes, but read as three separate primitives (`u16`, `u8`, `u8`) and with
no clamping:

- If any of the three is zero → no date.
- Otherwise the date must be a real calendar date with a year of at most
  9999. An impossible one (month 13, 30 February, 31 April, year 10000) makes
  Banking4 fail and abort the file load.
- 0001-01-01 is the zero timestamp and therefore "no date", as for
  `date_clamped`.
- End of chunk: see section 3, the three sub-reads fail independently.
- **Examples:** `E7 07 0B 05` → 2023-11-05; `E7 07 00 05` → no date.

### 4.13 `ticks`

A full timestamp.

- **Size:** 8.
- **Layout:** `i64`.
- **Value:** the number of 100-nanosecond intervals since
  0001-01-01T00:00:00 in the proleptic Gregorian calendar (the .NET tick
  count). Valid range 0 ..= 3155378975999999999. Out-of-range values (negative
  included) make Banking4 fail.
- **No date:** `0`, which is also the default for a missing field. The upper
  bound is `FF 3F 37 F4 75 28 CA 2B` on the wire.
- **Time zone is not on the wire.** The writer stores the raw tick count of
  whatever timestamp it is given, without converting it, and the marker saying
  whether that timestamp was UTC or local time is dropped. The reader returns
  an unqualified timestamp. Whether a given field holds UTC or local wall
  clock time is therefore a property of the field, not of the encoding (open
  question 1).
- **Conversion:** `unix_seconds = (ticks - 621355968000000000) / 10_000_000`.
- **Example:** `00 39 8E B1 2C 39 DC 08` → 638448111300000000 ticks →
  2024-02-29T13:45:30.0000000.

### 4.14 `time_hms`

A time of day with one-second resolution.

- **Size:** 3.
- **Layout:** `hour`, `minute`, `second`, one byte each.
- **Null:** written as `FF FF FF`. The reader only tests the **first** byte:
  `hour == FF` means null whatever follows.
- **Not null:** `00 00 00` is midnight. A missing field (short chunk) is null,
  so "absent" and "midnight" are distinguishable, unlike numeric fields.
- **Range:** hour 0..23, minute 0..59, second 0..59. Anything else (other than
  the null marker) makes Banking4 fail and abort the file load.
- **Examples:** `0D 2D 1E` → 13:45:30; `FF FF FF` → null.

## 5. Conventions built on the primitives

### 5.1 Stamps: object ids and update stamps

A **stamp** is an `i32` counting **whole seconds since
2000-01-01T00:00:00 UTC**.

```
unix_seconds = stamp + 946684800
ticks        = stamp × 10_000_000 + 630822816000000000     (0x08C1220247E44000)
```

- Converting a time to a stamp truncates fractions of a second and maps any
  time before the epoch to 0.
- A stamp is UTC by definition, so it can be shown as an ISO 8601 time with a
  `Z` suffix. This is different from `ticks` (4.13), which carry no zone.
- Converting a stamp to a time is done with 64-bit arithmetic and no range
  check, so a negative stamp is a time before 2000 (`-1` →
  1999-12-31T23:59:59Z). The largest stamp is 2068-01-19T03:14:07Z.
- The same stamp encoding is used by the container for its own time stamps.
- Banking4's stamp-to-time conversion returns a timestamp marked as UTC. Its
  time-to-stamp conversion converts its argument to UTC first: a timestamp
  marked as local, **or not marked at all**, is taken as local wall-clock time
  of the machine. This matters only inside Banking4 (an unmarked `ticks`
  value fed to the conversion would be shifted by the local UTC offset); a
  reader converting stamps to times is not affected.

Every record starts with two stamps (they are the fields of the common base of
all record types, see the per-record documents):

- **Object id.** Generated as "the current stamp, or the highest id seen so far
  plus one, whichever is larger" (confirmed since, see 03, "Identity and time
  stamps"). Ids are therefore handed out in increasing order from one counter
  for all record types, which makes them unique across a vault as long as
  every writer saw the earlier ids. Nothing enforces that: when it loads a
  vault Banking4 only repairs a repeated id *within one collection file*
  (03), and two installations writing to copies of one vault could reuse an
  id. A reader should treat `oid` as unique per collection file at most, key
  per-account and per-instrument records by the owning id plus `oid`, and
  report duplicates. An id read as a time is
  the object's creation time **approximately**: when many objects are created
  within one second (an import, a statement download), the ids run ahead of the
  clock by one second per object. Banking4 itself displays this value as the
  creation date. Do not treat it as exact.
- **Update stamp.** The time the object was last modified, as a stamp.
  `-1` (`FF FF FF FF`) is the in-memory marker for "not stamped yet"; the
  writer replaces it by the current time when the object is saved, so it should
  not occur in a file, but a reader should accept it as "unknown".

**Stamp 0 is ambiguous.** As a stamp, 0 is 2000-01-01T00:00:00Z. But the
time-to-stamp conversion also stores 0 for every time before 2000 (see
above), so an update stamp of 0 says "at the epoch or at some unknown time
before it". `b4` therefore treats an update stamp of 0 like -1: the time of
the last change is unknown, and the database stores NULL. A change made in
the very second of the epoch is reported as unknown too; that is the price
of not presenting an unknown time as a date. This applies to the update
stamp only. An object id is an identifier first and is always kept as the
number it is, 0 included. (For the container's own time stamps, 0 means "not
set" as well, 01 section 3.4.)

Example: `FA 46 73 2D` → 762529530 → 2024-02-29T13:45:30Z.

### 5.2 Multi-line text in a `string`

Texts made of lines (payment purpose, additional information) are stored as a
single `string` with the lines joined by one LF (`0A`).

- **Writer:** no lines → null (`00 00`); otherwise the lines joined with LF.
- **Reader:** splits at CR, LF or CR LF, and **discards empty lines**. Lines
  are not trimmed; a line consisting only of spaces is kept. Only CR and LF
  separate lines: U+0085, U+2028, vertical tab and form feed are ordinary
  characters of a line.
- Asymmetry: the in-memory text can contain an empty line, which is written
  (as two consecutive LFs) but lost on the next read. Empty lines are rare in
  memory to begin with: when Banking4 adds a line to a text in the ordinary
  way it ignores an empty string, and only one dedicated operation creates an
  empty line.
- A text that consists of one empty line joins to the empty string, which is
  stored as `00 00` like "no lines". Two empty lines are stored as `01 00 0A`.
  Both read back as no lines.

## 6. Read/write asymmetries

Places where writing a value and reading it back does not give the same value,
or where the reader accepts more than the writer produces.

| Primitive | Asymmetry |
| --- | --- |
| `record_size` | writer: exactly the sum of chunk sizes. Reader: tolerates slack after the last chunk. |
| chunk header | writer: bit 30 always clear. Reader: ignores bit 30. |
| `bool` | writer: `00`/`01`. Reader: any non-zero is true. |
| `tristate` | writer: `00`/`01`/`02`. Reader: anything above 1 is unknown. |
| `u16` | writer keeps the low 16 bits of a 32-bit value; reader returns 0..65535. Negative and oversized values do not round-trip. |
| `string` | empty is written like null and reads back as null. Unpaired surrogates are written as U+FFFD. Reader accepts the long form for short strings. The number of U+FFFD read for malformed UTF-8 depends on the runtime Banking4 runs on. |
| `blob` | empty is written like null and reads back as null. Over-long: fatal, where `string` is lenient. |
| `code(n)` | writer truncates to `n` and to one byte per character; reader trims NUL padding and surrounding white space; empty reads back as null. |
| `decimal` | none (bit-exact, scale preserved). |
| `date_raw` | writer from a timestamp drops the time of day; reader accepts unvalidated and partial values. |
| `date_clamped` | writer drops the time of day; reader clamps values the writer could never have produced. A timestamp on 0001-01-01 reads back as "no date". |
| `date_strict` | writer drops the time of day; reader fails on impossible dates the writer could never have produced. A timestamp on 0001-01-01 reads back as "no date". |
| `ticks` | UTC/local marker dropped on write; the value read is unmarked. |
| `time_hms` | writer: `FF FF FF` for null. Reader: only the hour byte decides. |
| multi-line text | empty lines are written but dropped on read; CR inside a line splits it on read. |

## 7. What Banking4 treats as fatal

Any of these aborts the loading of the affected file. Records decoded before
the error stay in the collection, the rest of the file is not read, and the
user is shown a notification *(this consequence is not confirmed; for the
errors themselves see the reader cases of 10.1)*:

- `record_size` below 4 or above `0x4000_0000`; record body truncated.
- `chunk_size` below 4, a chunk running past `record_end`, or no room for a
  chunk header where one is expected.
- A `blob` longer than the rest of its chunk.
- A chunk left unconsumed with `more_chunks` set (reported as a compatibility
  problem rather than as corruption).
- A `decimal` with an invalid `flags` word; an impossible `date_strict`; an
  out-of-range `ticks`; an out-of-range `time_hms`.
- A long-form `blob` length large enough to wrap (section 3); for a `string`
  with such a length, the next read of one or more bytes in the same chunk.

For the first four groups Banking4 raises the format's own errors (corrupt
stream, or compatibility for the unread chunk). The last two groups are
ordinary runtime errors raised while the value is built; when it loads a file
Banking4 catches those as well and reports them to the user as a corrupt file
*(not confirmed)*.

Not fatal: a chunk shorter or longer than expected, an over-long or undecodable
`string`, slack inside a record, 1 to 3 stray bytes at the end of the file, a
set bit 30 in a chunk header.

## 8. Reader rules for `b4`

`b4` only reads. It should accept everything Banking4 accepts, decode it to
the same values, and be at least as tolerant where tolerance cannot change the
meaning of well-formed data.

1. **File.** Loop: stop when fewer than 4 bytes remain; otherwise require
   `4 <= record_size <= 0x4000_0000` and `record_size` bytes available, else
   report the file as corrupt at that offset. Do not try to resynchronise.
2. **Chunks.** Mask the header with `0x3FFF_FFFF` for the size and test bit 31
   for the flag; ignore bit 30. Require `chunk_size >= 4` and the chunk to lie
   inside the record.
3. **A short chunk means defaults.** Implement every primitive with the exact
   rule of section 3: check against `chunk_end`, return the default, leave the
   cursor alone. Do not turn a short read into an error. Mirror the special
   cases too (`date_strict` as three reads; the `string` cases), because they
   determine which bytes later fields see.
4. **Unknown trailing bytes of a chunk are skipped.** Advancing to the next
   chunk starts at `chunk_end` regardless of the cursor; finishing a record
   needs no check.
5. **Advancing without a next chunk is a no-op.** Keep reading at the current
   cursor in the current chunk, exactly like Banking4.
6. **Extra chunks are tolerated.** Where Banking4 rejects the whole file for
   an unconsumed chunk, `b4` decodes the chunks it knows, skips the rest, and
   records a warning naming the file and record. This is a deliberate
   deviation: for a read-only tool, partial data from a newer vault is more
   useful than none. A strict mode that fails instead is reasonable.
   The tolerance covers structurally valid chunks only. Every chunk that a
   record announces is checked by rule 2, the ones the decoder never visits
   included, and also when the record is undecodable for another reason
   (rule 8). A chunk whose header is missing or whose size is below 4 or
   reaches past the record is a broken chunk structure: the file is corrupt
   at that record and reading it stops there, as in rule 1, with the records
   before it kept. Skipping such a record and carrying on would trust a
   `record_size` that the record's own contents contradict.
7. **Strings.** Decode as UTF-8 with replacement of invalid sequences; map
   length 0 to null and keep null distinct from a (never occurring) empty
   string in the internal model, or normalise both to one representation
   consistently. Treat a negative long-form length as corrupt. For an
   over-long string return null and poison the rest of the chunk, as
   Banking4 does: the cursor then stands at `record_end`, which can lie beyond
   `chunk_end`, so `remaining` must be computed in signed arithmetic or
   clamped at zero, and every later read in that chunk returns its default
   without moving (section 3). A test must cover this with a chunk that is
   followed by another chunk. Decide "over-long" by comparing the length with
   `remaining`, never by adding it to the cursor, so that an absurd length
   cannot wrap (section 3); the same for blobs. The number of U+FFFD produced
   for malformed UTF-8 need not match Banking4's.
8. **Values Banking4 would fail on** (invalid decimal flags, impossible
   strict date, bad time, out-of-range ticks, over-long blob): report the
   record as undecodable with file, record index and field, rather than
   guessing. Banking4 never writes such values.
9. **`date_raw` never fails and never costs a record.** At the wire layer keep
   year, month and day as read, a negative year included, and offer the
   clamping conversion of 4.12 for whoever wants Banking4's own reading. In
   the database a value that is not a real calendar date is NULL plus one
   warning per table and column with a count; the record stays (4.12). For
   all three date readers, 0001-01-01 is "no date", exactly as in Banking4.
10. **Stamps** are converted with `unix = stamp + 946684800`; show object-id
    times as approximate. An update stamp of 0 or -1 is "unknown" (5.1).
11. **Counted lists.** Some records store a count followed by that many
    elements (03 and 04 name them; the records of 05 have none). Never
    allocate by the count. Before reading the elements, refuse a count that
    is larger than the bytes left in the chunk divided by the smallest size
    one element can have, and do that comparison as a division or in checked
    arithmetic, never as `count * size` in 32 bits, which wraps. What a zero
    or negative count means is said for each list where it is specified.

## 9. Open questions

1. **UTC or local time in `ticks` fields.** The encoding drops the marker.
   Which convention each timestamp field follows has to be settled per field.
2. **Bit 30 of the chunk header.** The reader masks it off and the writer of
   the Banking4 version described here never sets it. Whether any program
   version ever set it, and with what meaning, is unknown.
3. **Older layouts.** Only one build of Banking4 is described. The "advance
   without a next chunk keeps reading in place" behaviour would be consistent
   with an older layout in which the fields now living in the second chunk
   followed the base fields in the same chunk, but this was not confirmed.
   That real vaults contain records in a legacy layout, which the Banking4
   version described here still reads, is settled: one real vault holds
   bookings and payments in the legacy single-chunk layout of the payment
   family, and `b4`'s output for them was compared with data exported from
   Banking4 with no differences found (04, open question 1). Which program
   versions wrote them, and what other old layouts exist, is still unknown.
4. **Dormant XML hooks.** Banking4 is able to read and write the categories
   collection as XML, although the file is binary and the normal loading of
   a vault never takes that path *(not confirmed)*. Whether old vaults can
   contain an XML categories file was not established.
5. **Partial dates.** `date_raw` can carry year-only or year-month values.
   No record type of documents 03 to 05 produces them on purpose, and
   whether any real vault holds one is unknown. A reader must still carry
   them at the wire layer (section 8, rule 9); the count in the warning of
   4.12 is what will answer the question.
6. **Signed `u16` fields.** Settled: the booking's `gvc` and `gvc_extension`
   are the only ones (4.4). The two fields themselves are confirmed on test
   files produced with Banking4 (04); that no other field of the Banking4
   version described here does the same is not confirmed.
7. **Not confirmed.** The error handling around a whole load (a missing file
   is an empty collection; which errors are caught and how they are reported)
   and the *reading* side of the Base64-embedded record form are not
   confirmed. See section 10. (Object-id generation, which this list used to
   include, has been confirmed for document 03.)
8. **Platform differences.** Three details are decided by the runtime
   Banking4 runs on rather than by Banking4 itself: the number of U+FFFD for
   malformed UTF-8 (4.8), the white space set used when trimming a `code(n)`
   (4.9), and the kind of runtime error raised for invalid decimals, dates,
   times and tick counts (section 7). They may differ between the platforms
   Banking4 runs on and are not confirmed for all of them. None of them
   affects data Banking4 writes.

## 10. Verification

This section says which test files the repository holds for this document,
what they cover, what was corrected in the document, and what is not
confirmed. It concerns the Banking4 version described here.

### 10.1 Test files

The test files are in `tests/fixtures/wire/`. They hold synthetic data only.

- 40 samples produced with Banking4, with 266 operations, 257 values and 9
  chunk changes (`*.bin`, `MANIFEST.json`): for every value what was written,
  what is read from the bytes, and the cursor after the read. Two of the
  samples are whole collection files. The manifest carries the bytes as hex
  only for samples up to 2048 bytes; larger ones are identified by size and
  SHA-256 of their file. The manifest also holds four tables for 4.12 and
  section 5: stamps to times, times to stamps, raw dates to timestamps, and
  multi-line texts joined and split.
- 579 reader cases (`reader-cases.tsv`), cases with their expected results:
  one short value of every primitive ending after 0, 1, 2, ... of its bytes
  (strings and blobs in both length forms) in three surroundings (the chunk
  ends the record; unused bytes follow inside the record; another chunk
  follows), encodings the writer never produces, malformed chunks, and whole
  collection files. Each case gives the expected value and the full reader
  state (cursor, chunk bounds, `more_chunks`, `consumed_size`) after every
  read, or the expected error.

`tests/wire.rs` runs `b4`'s reader against all of these files and against the
byte examples printed in this document. Two things are compared loosely on
purpose: the number of U+FFFD for malformed UTF-8 (4.8), and the sign of a
negative zero `decimal` (4.7). In five reader cases `b4` differs from the
expected result on purpose: they are the negative and the absurd `string`
and `blob` lengths that section 8, rule 7 tells a reader not to reproduce.
The test names `b4`'s own result for each of them. The file cases are run
with the rules of 1.2, under which any failure and any chunk left unread
ends the file; what `b4`'s more tolerant loader makes of the same files
(section 8, rules 6 and 8) is tested separately.

### 10.2 Confirmed

The test files of 10.1 cover:

- **Files (1.2, 1.3):** records back to back with a signed size that excludes
  itself; an empty file; 1 to 3 stray bytes at the end ignored; sizes 0, 3,
  negative and `0x4000_0001` rejected as corrupt; size 4 accepted; a missing
  or truncated body is corrupt; slack inside a record ignored; an unread chunk
  with `more_chunks` set gives the compatibility error, also after the decoder
  has advanced once; the loop stops at the first error; an object that
  declines to be written leaves no trace; the writer's `record_size` is the
  sum of the chunk sizes.
- **Chunks (2):** header layout, size including the header, bit 30 ignored on
  read and never written, bit 31; all corrupt cases of opening the first and a
  following chunk; advancing skips unread bytes; advancing without a next
  chunk changes nothing; closing checks nothing; reads never cross
  `chunk_end`; `consumed_size`; a record at an offset inside a larger buffer,
  with positions counted from the start of the record body and bounds taken
  from the record, not from the buffer.
- **End of chunk (3):** every row of the tables, for every primitive, at every
  length at which a short value can end, including the `date_strict`
  sub-reads, the incomplete long-form length, the jump to `record_end` for an
  over-long string (told apart from `chunk_end` by the cases with slack or a
  following chunk), the backwards move for a negative string length, the fatal
  over-long blob, and the wrap-around for absurd lengths.
- **Primitives (4):** all layouts in both directions. In particular: `u16`
  written from negative and oversized 32-bit values; decimals with scales 0 to
  8 and 28, both signs, negative zero, all 96 magnitude bits, and every kind
  of invalid `flags` word; strings null, empty, ASCII, with umlauts, the euro
  sign, Cyrillic, CJK, a supplementary-plane character, a byte order mark,
  NUL, CR and LF, unpaired surrogates, and of 65534, 65535, 65536 and 70000
  bytes (the long-form threshold counts bytes); blobs null, empty, small and
  of 65534, 65535 and 70000 bytes; `code(n)` with each of the 256 byte values
  at both ends, NUL padding, NULs elsewhere, over-long and non-Latin-1 input
  to the writer, width 0; GUID byte order; the three date readers on the same
  bytes including partial, impossible, out-of-range and negative-year values;
  both date writers including the zero timestamp and 0001-01-01; tick counts
  at both bounds and outside; that a timestamp's UTC/local marker is not
  stored and the value read is unmarked; `time_hms` null marker and range.
- **Conventions (5):** stamp to time and back, including negative stamps, the
  largest stamp, times before the epoch and truncation of fractions; raw date
  to timestamp conversion; joining and splitting of multi-line text including
  empty lines and other line-break-like characters.

### 10.3 Corrections made

Earlier versions of this document were corrected in these points:

1. **Absurd string and blob lengths (3, 7, 8).** The over-long test wraps in
   32-bit arithmetic; a length near 2 GiB passes it. Previously the document
   said every over-long string reads as null with the cursor at `record_end`
   and every over-long blob is a corrupt-stream error.
2. **Order of the string cases (3).** Made explicit as six steps, including
   where the cursor is after a zero length and from where the backwards move
   of a negative length is counted; the same for blobs.
3. **The zero timestamp in date fields (4.12, 6, 8).** Added what the writers
   do: the zero timestamp is written as `00 00 00 00`, any other moment of
   0001-01-01 as `01 00 01 01`, and both read back as "no date". The document
   had not said what the writer does with a default timestamp.
4. **`date_strict` (4.12).** A year above 9999 fails like any other impossible
   date; previously only month and day were mentioned.
5. **`date_raw` (4.12).** Changing a single component of a date in memory
   cannot store a zero; partial values arise from setting one component of a
   null date. Values that clamp to 0001-01-01 convert to "no date".
6. **`code(n)` trimming (4.9).** The white space set is now given exactly
   (`09`..`0D`, `20`, `85`, `A0`) instead of "and similar", with the
   consequence of the two-step order for a NUL next to a space.
7. **Malformed UTF-8 (4.8, 6, 9).** The count of replacement characters is a
   property of the runtime Banking4 runs on, not of the format; the document
   had implied one U+FFFD per invalid sequence.
8. **Chunk and record limits (1.2, 2.1).** The largest chunk is `0x3FFF_FFFF`
   bytes and a header with only bit 30 set is a corrupt size 0; the record
   size bound is inclusive.
9. **Opening a chunk (2.3)** now says that it sets `more_chunks`.
10. **Stamps (5.1).** Added the UTC marker of converted stamps and the
    local-time interpretation of unmarked times.
11. **Multi-line text (5.2).** Only CR and LF split; how empty lines get into
    memory at all; a text of one empty line is stored like no text.
12. **Kinds of failure (7).** Told apart the format's own errors from runtime
    errors raised while a value is built.

None of these changes how well-formed data written by Banking4 decodes.

### 10.4 Not confirmed

- **Loading around the file loop.** The file loop itself is covered (10.2).
  What Banking4 does around it when it loads a file from a vault (opening
  the file, catching errors, logging them and notifying the user) is not
  confirmed. That includes "a missing file is an empty collection" and
  "records decoded before an error stay in the collection".
- **The container's stream.** The test files of 10.1 are records and
  collection files, not containers. That the container's file stream only
  returns a short read at the true end of a file (1.2, step 1) rests on
  document 01.
- **Reading the Base64 record form (1.4).** That reading this form makes no
  end-of-record flag check is not confirmed.
- **Binary versus XML selection (1.1)** and the dormant XML hooks (open
  question 4).
- **Object-id generation (5.1)** is not covered by the test files of this
  document. It has been confirmed for document 03 since (03, "Identity and
  time stamps").
- **Which fields use which reader.** Signed `u16` fields, `ticks` fields in
  UTC or local time, partial `date_raw` values and non-negative decimals are
  per-record matters.
- **Real data.** The test files of this section hold nothing from a real
  vault. That real collection files decode with these rules is confirmed
  against data exported from Banking4: on one real vault, written by a newer
  program version than the Banking4 version described here, `b4`'s output was
  compared with data exported from Banking4 for all eleven record types the two
  have in common, and no differences were found (README, "Real vaults"). That
  vault holds records of the legacy single-chunk layout and records with unread
  bytes at the end of a chunk; whether it holds partial dates, over-long
  strings or extra chunks is not known, so the rules for those still rest
  on the test files of 10.1.
- **Other builds and platforms.** Only one build of Banking4 is described
  (open question 3), and the three details of open question 8 are not
  confirmed for all platforms Banking4 runs on.
