# Demo vault

`demo.sub` is a complete Banking4 data vault with invented content: the
finances of a fictional German household from 2024-01-01 to 2025-06-30. It is
the end-to-end input of the test suite (decrypt, then query), the source of the
examples in the documentation, and the test that `b4` never reads bank access
data.

**Password: `demo`**

Everything in it is made up. No person, bank, company, account or security in
it exists:

- Bank codes start with `9999`, in a range no German bank code uses, and the
  IBANs built on them have correct check digits.
- BICs have `0` as the second character of the location code, the form SWIFT
  reserves for test addresses.
- ISINs start with `ZZ`, a country code no numbering agency issues, and have
  correct check digits.
- SEPA creditor identifiers have correct check digits over an invented
  national part.
- The holders are Erika and Max Mustermann of Musterstraße 12, 12345
  Musterstadt.

The vault was produced with Banking4.

## Container

| | |
|---|---|
| Cluster size | 512 bytes |
| Content cipher | AES-256 (`cipher_id` 3) |
| Key stretching | 100000 rounds |
| `security_flags` | 2 |
| `type_guid` | `6ab4f284-82a0-46fd-83f4-438632912ba7`, the content type of Banking4 data vaults |
| Device GUID, access token | none |

All five values were chosen, none was left to a default. Cluster size, cipher
and round count are the ones asked for; flags value 2 and the type GUID were
picked because the one real vault that is known has them (the container
fixture `v06` has the same combination). All files are stored encrypted.

## Content

| Collection | File | Records |
|---|---|---|
| account groups | `AcctGroups` | 2 |
| accounts | `AcctsNew` | 6 |
| categories | `Categories` | 30 |
| payees | `Payees` | 30 |
| mandates | `Mandates` | 1 |
| exchange rates | `ExchangeRatesBIN` | 10 |
| stock instruments | `StockInstruments` | 4 |
| stock prices | `Stock<oid>/StockPrices`, 4 files | 240 (60 trading days each) |
| bookings | `Acct<oid>/Stmt`, 5 files | 634 |
| payments | `Acct<oid>/Paymts` of the Girokonto | 2 |
| standing orders | `Acct<oid>/Stos` of the Girokonto | 3 |
| portfolio items | `Acct<oid>/Portf` of the Depot | 3 |
| portfolio transactions | `Acct<oid>/PortfTxns` of the Depot | 2 |
| tax exemptions | `Acct<oid>/AcctTaxExemptions` of the Depot | 1 |

| Account | Kind | Bookings | Balance | Notes |
|---|---|---|---|---|
| Girokonto | bank | 427 | 14733.44 | salary, rent, utilities, insurance, groceries, fuel, cash withdrawals, the monthly transfer to savings; one split, one batch, three pending entries |
| Tagesgeld | bank | 26 | 18793.65 | monthly deposits and quarterly interest |
| Kreditkarte | credit card | 123 | -250.34 | card charges, settled monthly from the Girokonto; two charges in US dollars; two pending entries |
| Depot | portfolio | none | 0 | three positions, no currency of its own |
| Bargeld | cash | 55 | 391.74 | the withdrawals arriving, and what was spent |
| Altes Sparbuch | bank, hidden | 3 | 0.00 | closed in January 2025 |

The two groups are `Alltag` (Girokonto, Kreditkarte, Bargeld) and `Vermögen`
(Tagesgeld, Depot); the hidden account is in no group.

Things the data contains on purpose:

- **Categories** in three levels (`Wohnen:Nebenkosten:Strom`), with non-ASCII
  letters (`Mobilität:ÖPNV`), keyword rules and budgets per month, quarter and
  year, and the three special categories every catalogue has, under their
  German names: `(Umbuchung)`, `(nicht auswerten)`, `(Umsatzsteuer)`. Budget
  amounts are stored positive; which sign the application uses for an expense
  budget is an open question of the format description.
- **Transfers between the accounts** appear on both sides with the category
  `(Umbuchung)`: savings rate, cash withdrawal, credit card settlement, the
  closing of the old savings book. The opening balances are bookings with the
  category `(nicht auswerten)`.
- **33 booked entries without a category** (about 5 %).
- **Five pending entries** (`booking_status` 1) at the end: three of the
  Girokonto without a booking date, two of the Kreditkarte dated 2025-06-30.
- **One split**: a purchase of 84.37 on 2024-11-14 whose three parts carry
  categories of their own. **One batch**: a collective booking of 244.88 on
  2025-03-18 for three transfers. A master points at itself with `batch_oid`,
  its parts point at it and follow it directly.
- **Purposes** of one to four lines; 178 bookings with text outside ASCII
  (umlauts, `ß`, `€`) in a name, purpose, category or note; and one purpose of
  19 lines and 1536 bytes (the utility's yearly statement of 2025-02-06).
- **Payments**: one that was sent and matched with its booking
  (`reconciled_booking_oid`), one that was entered and not sent.
  **Standing orders**: rent and savings rate held at the bank, a quarterly
  membership fee kept in the application.
- **Securities**: four instruments, one of them with a stock split and one
  that is no longer held (it was sold, so no portfolio item refers to it; the
  application's clean-up would remove such an instrument). Each item's current
  price is the last price of its instrument's series.

### The balance rule

Every account starts at 0.00. For the kinds that have statements (bank, cash,
credit card) the stored `statement_balance` is the sum of the signed amounts of
the booked entries (`booking_status` 0), credits positive and debits negative,
with split and batch masters left out, because a master stands for its parts.
Pending entries do not count. The balance time is the day of the last booked
entry. This holds for every account of the vault, so the running balance
Banking4 derives before the first entry of every account is 0.00.

## Entries `b4` must not read

Each of these files holds nothing but the ASCII text
`B4-DECOY-MUST-NOT-BE-READ`, repeated back to back without separator. They are
not valid records, and that is intended: a reader that decrypts one of them has
a defect, and the marker would show up in its output.

| Entry | Why it is not read |
|---|---|
| `Contacts`, `EbicsContacts`, `ContactsHTML`, `OnlineContactKeys`, `SpoolerContacts/Contact1` | never read: the stored bank accesses |
| `Outbox`, `OutboxData/800391600.xml` | never read: the outbox holds user ids, TANs and bank contacts of orders |
| `ModelSettings` | never read: the settings hold a crypto key and API tokens among preferences |
| `OnlineTraces` | never read: bank protocol logs |
| `FutureCollection`, `Acct757073100/FutureThing` | names the format description does not know: skipped with a warning |

The first four rows are the nine entries of the root directory that `b4`
skips by name before a byte of their content is read (`docs/format/README.md`,
"Never"). The manifest gives all of them the class `bank_access`. They are
only counted; no warning names them.

In a vault that Banking4 works with, `Contacts`, `EbicsContacts` and
`ContactsHTML` are directories, like `SpoolerContacts`. Here they are files, so
that the vault has both forms of an entry that is skipped by name.

`Accts` is the legacy name of the account file. It holds the single placeholder
record of account kind 0 that a migrated vault keeps there, with the values
Banking4 gives it. Because `AcctsNew` exists, a reader ignores `Accts`.

## MANIFEST.json

The manifest does not say what was meant to be written. It gives the expected
result of reading the finished vault. Its `notation` object explains every
key. In short:

- `vault`: password, the header values, and under `not_reproducible` what
  belongs to this one file (see below).
- `entries`: every entry of the container with its size, the SHA-256 of its
  decrypted content, and whether `b4` reads it (`b4_reads`, `class`,
  `b4_warns`); for a collection its record type and record count.
- `totals`: records per record type.
- `accounts`: per account the stored balance, the number of booking records,
  and counts and exact sums of credits and debits, separately for booked
  entries, pending entries and masters, with the first and last booking date.
- `categories`: the catalogue, and per category name how many bookings carry
  it and what they add up to.
- `reader_changes`: the fields that read as another value than was written,
  by the rules of the format description (the balance of an account without
  statements, the online balance of a cash account, the budget of the special
  categories, the next date of a standing order held at the bank).
- `samples`: 31 records of every record type in full, under the field names
  and in the value forms of `tests/fixtures/records`.
- `banking4_load`: the expected result of loading the collections by
  Banking4's rules. For this vault: nothing dropped, nothing added, nothing
  changed, and every file whose order is given is already in Banking4's order
  (the order of standing orders depends on the day and is not given).
  `loads_reported_as_failed` names `ModelSettings`: Banking4 reads its
  settings on its own while it loads the exchange rates, and in this vault
  that entry is not a settings file, so the load of `ModelSettings` fails.
  This says nothing about the collections.

## Regenerating

Nothing in this repository builds the vault. `demo.sub` and `MANIFEST.json`
belong together; do not edit either.

**Reproducible:** every collection file, byte for byte, and with it every
`sha256` under `entries` and every number and sample in the manifest. The
content comes from a fixed seed and fixed dates; nothing in a record depends on
the clock or on machine settings. The expected values hold for German as
Banking4's language, `EUR` as the application's default currency and the
Professional edition (`banking4_settings` in the manifest; the default product
variant, which is not the full edition, loads no batches and no portfolio
transactions).

**Not reproducible:** the container around the content. Banking4 draws a new
salt, content key and file GUID for every container and stamps it with the
current time, so the bytes of `demo.sub`, its SHA-256 and the values under
`vault.not_reproducible` belong to this one file, while its size depends on
the content only. A test must take those values from the manifest that was
committed with the vault and must not expect them to hold for another copy of
the same content.
