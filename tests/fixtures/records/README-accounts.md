# Record fixtures: record base, account, group, filter, setting

Synthetic records of five record types, for testing the record decoders
against `docs/format/03-model-and-accounts.md`:

| Files | Record | Records | What varies |
|---|---|---|---|
| `record_base.bin`, `record_base.json` | the common record header on its own | 6 | zero, negative and extreme ids and update stamps |
| `account.*` | account | 24 | every kind; every string null, empty and set; the internal transfer list absent, empty, with null and blank entries; all retention period shapes; time stamps with and without a time of day and at the limits; a 70000-byte string; flag sets with undefined bits and wider than they are stored; the cases Banking4 changes on reading (cash, crypto currency, the `120300` bank codes, kinds without statements) |
| `account_group.*` | account group | 7 | each flag, positions outside 16 bits, null, empty and multi-line strings |
| `booking_filter.*` | saved booking filter | 15 | every period, amount mode and transaction kind (and values outside the enumerations), date ranges Banking4 sanitises on reading, category lists with empty, blank and missing entries |
| `setting.*` | settings entry | 11 | missing and empty name or value, multi-line and non-ASCII text, a 70000-byte value, a repeated name |

They were produced with Banking4 from made-up values. Nothing comes from a
vault; every name, number, IBAN-like string and date is invented.

## The files

`<record>.bin` is a collection file as it would sit in a vault: records back
to back, each preceded by its size (`docs/format/02-records-wire-format.md`,
section 1.2).

`<record>.json`:

| Key | Meaning |
|---|---|
| `record_type`, `file` | the record type and the `.bin` file |
| `records[].about` | what the record is for |
| `records[].offset`, `record_size` | where the record's size prefix is in the file, and the value of that prefix |
| `records[].set` | the record's fields in memory before it was written |
| `records[].read` | the expected result of reading the bytes: the record's fields in memory |

A decoder is correct when, for every record, what it decodes equals `read`.
`set` shows what was written; where `set` and `read` differ the
format is lossy or Banking4 changes the value on reading (an empty string
comes back as null, a cash account loses its online banking fields, a 45-day
retention period becomes three months, ...). The document explains each of
these.

`set` and `read` hold the in-memory fields under this project's names, not the
fields on the wire. A few wire fields have no counterpart: the account's four
day-precision dates are superseded by the tick-precision time stamps of the
same name, `holder_address` appears as `holder_street` and `holder_town`, the
retention periods appear as a unit and a count, and the unused fields and the
record version are not shown.

## Values

| Kind | In the JSON |
|---|---|
| integers, enumerations, flag sets | numbers |
| amounts and rates (`decimal`) | strings with the exact scale and sign as stored: `"-28.2800"`, `"-0.00"` |
| time stamps and dates | `"YYYY-MM-DDThh:mm:ss.fffffff"`, or `null` for "no time" |
| GUIDs | lower-case text form |
| retention periods | `{"unit": 0\|1\|2, "count": n}`; count 2147483647 is "unlimited", 0 is "not set" |
| string lists | arrays, `null` for "no list" |
| a long string made of one 10-character unit | `{"repeat": "0123456789", "times": 7000}` |

Not in the JSON, because they are derived and depend on the day or the
machine: the account's three purge dates and the filter's parsed search
amount.

## Regenerating

Nothing in this repository builds these files. Do not edit them: each `.json`
describes exactly the bytes of its `.bin`.
