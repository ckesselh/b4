# Booking, payment and standing order fixtures

Synthetic records for `docs/format/04-payments-and-bookings.md`. Do not edit
them. 144 of the 255 records are in a form Banking4 writes and were produced
with Banking4. The other 111 are cases in forms Banking4 does not write as a
whole record, with their expected results (`built_by` below says which is
which). Every value is made up: strings name the field they belong to
(`counterparty_name#12`), numbers and dates are picked so that no two fields
of a record look alike.

| File | Content |
|---|---|
| `booking.bin`, `payment.bin`, `standing_order.bin` | A collection file as it would be stored in a vault (`Acct<oid>/Stmt`, `Acct<oid>/Paymts`, `Acct<oid>/Stos`): records back to back, each a 32-bit little-endian size followed by the record. |
| `booking.json`, `payment.json`, `standing_order.json` | One entry per record of the collection file, in file order, and the expected result of loading the whole file. |
| `booking.rewritten.bin`, `payment.rewritten.bin`, `standing_order.rewritten.bin` | Collection files with the expected result of writing a record again after reading it, for the records where that differs from the bytes that were read. |

## The JSON files

The top level names the collection file with its size and SHA-256, the
language the expected values hold for (it matters for pseudo category names),
the application default currency that applies to accounts without one, and a
`notation` object that explains every key. In short, per record:

- `name`, `about`: what the record is there for.
- `built_by`: `record_writer` for a record in a form Banking4 writes as a
  whole, produced with Banking4, in which case `set` holds the in-memory
  values before writing; `primitive_writer` for a case in a form Banking4
  does not write as a whole record, in which case `wire` lists the values in
  wire order.
- `offset`, `record_size`, `chunk_sizes`: where the record is in the `.bin`
  file.
- `read`: the expected result of reading the bytes for a bank account in EUR:
  every in-memory field after the adjustments Banking4 makes on reading
  (`fields`),
  values Banking4 computes from them (`derived`: effective direction, signed
  value, displayed currency and category, ...), whether an unread chunk is
  left, and whether writing the record again gives the same bytes. If
  Banking4 rejects the record, `error` names the kind of failure instead.
- `read_other_accounts`: the expected result of reading the same bytes for
  other accounts; only the values that differ are listed. Bookings have one
  for a cash account, a credit card account, a PayPal account and a bank
  account without a currency; payments and standing orders for a PayPal
  account and a bank account without a currency.

`collection_load` at the end is the expected result of loading the file
without the records named in `rejected_by_reader`: the records in the order
Banking4 puts them in, with the values it changes on the way (legacy states
and flags of payments, batch ids, batch positions), and the records it
discards. Banking4 rejects the records in `rejected_by_reader` on reading and
stops loading a file at the first such record.

Field names are this project's names from the document. `set` and `fields`
describe the record in memory, so they contain `purpose_lines` and
`scheme_type` where the wire has `purpose_text` and `class_and_direction`, and
a few fields that are never stored (`notation.memory_only`).

## What is in them

- Every stored field with a value of its own, and every field at its "nothing"
  value.
- Every member of every enumeration and bit set at least once, and values no
  member stands for.
- Bookings: booked, pending and information entries; missing booking date;
  split master with slaves, batch master with slaves, masters and slaves with
  broken links; a cross entry link and the other memory-only fields; colour
  flags out of range; negative business transaction codes; dates with a time
  of day, at the limits, partial and impossible; multi-line purposes with
  blank lines and carriage returns; categories that need normalising; text
  outside ASCII; odd fixed-width codes; odd amounts.
- Payments: every scheme type, stored and derived; the direction bit against
  the scheme; missing currency for every scheme; batch master and slaves in
  the current and in the legacy form; every status code text; payee
  verification texts, well-formed and not; unused fields carrying values.
- Standing orders: every state bit; local and not local (the stored next
  execution date); weekly and monthly schedules; unused fields carrying values.
- For all three: the legacy single-chunk form with plain and with tagged
  purpose lines, a first chunk without the extension fields, and chunks that
  end early.
