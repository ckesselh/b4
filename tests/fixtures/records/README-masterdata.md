# Record fixtures: master data and portfolio

Synthetic records of twelve record types, for testing the record decoders
against `docs/format/05-master-data-and-portfolio.md`:

| Files | Record | Section |
|---|---|---|
| `payee.bin`, `payee.json` | payee | 3.1 |
| `payer.*` | payer | 3.2 |
| `category.*` | category | 3.3 |
| `mandate.*` | mandate | 3.4 |
| `exchange_rate.*` | exchange rate | 3.5 |
| `stock_instrument.*` | stock instrument | 3.6 |
| `stock_price.*` | stock price | 3.7 |
| `portfolio_item.*` | portfolio item | 3.8 |
| `portfolio_transaction.*` | portfolio transaction | 3.9 |
| `tax_exemption.*` | tax exemption | 3.10 |
| `asset.*` | asset | 3.11 |
| `account_document.*` | account document | 3.12 |

The well-formed records were produced with Banking4 from made-up values, so
their bytes are what Banking4 really writes. The other records (values outside
the enumerations, values wider than their wire width, partial and impossible
dates) are cases with their expected results. Nothing comes from a vault;
every name, number and date is invented.

## The files

`<record>.bin` is a collection file as it would sit in a vault: records back
to back, each preceded by its size (`docs/format/02-records-wire-format.md`,
section 1.2).

`<record>.json` describes it:

| Key | Meaning |
|---|---|
| `record`, `specification` | the record type and where it is specified |
| `collection_file`, `collection_size`, `collection_sha256` | the `.bin` file |
| `reader_context` | what the `read` values hold for: language, the application's default currency and country, and for account-bound records the currency of the account |
| `fields` | the field names used below, in a fixed order |
| `fields_in_memory_only` | fields that exist in the application's memory but are not stored; `written` gives them a value and `read` shows that it does not come back |
| `records[]` | one entry per record |
| `records[].about` | what the record is for |
| `records[].offset`, `record_size`, `record_hex` | where the record's size word is in the file, the size, and the record bytes without the size word |
| `records[].written` | the values the record held in memory when it was written |
| `records[].read` | the expected result of reading the bytes: the values the record then holds in memory |

`written` and `read` hold the **in-memory** values, under this project's field
names. Most of them are the wire fields of the specification one to one. Where
the specification says that Banking4 transforms a value on reading or
writing, the two differ from the wire and from each other, and that difference
is the point of the fixture:

| Record | In memory | On the wire |
|---|---|---|
| payee | `street`, `town` | `address` (section 3.1.1) |
| payee | `bank_street`, `bank_town` | `bank_address` |
| category | `rules` (canonical text) | `rules` as stored; `layout_version` and the two unused slots exist on the wire only |
| stock instrument | `isin` (object, see below) | `isin` (text) |
| stock instrument | `splits` (list) | `splits` (text, section 3.6.1) |
| portfolio item | `additional_info` (list of lines) | one text |
| asset | `amount`, `debit` | `signed_amount` (section 3.11.1) |

A decoder test therefore reads the `.bin`, applies the "After reading" rules of
the specification and compares with `read`. Comparing the raw wire values with
`written` (through the "Writer" rules) checks the field order independently.

## Value notation

| Kind | JSON |
|---|---|
| text | string; `null` for "no value". An empty string in `written` is an empty text in memory, which is written as "no value" |
| numbers, enumerations, flag sets | number (the numeric value, also for values that are no member) |
| decimal | string with the stored number of decimals, for example `"1.10"`; a negative zero is `"-0"` or `"-0.00"` |
| calendar date | `"yyyy-mm-dd"` with the three components as stored, even if they do not form a real date (`"2024-00-00"`); `null` for "no date" |
| timestamp | `"yyyy-mm-ddThh:mm:ss.fffffff"`, in `written` followed by `Z` or `L` if the value was marked as UTC or local time (the marker is not stored); `null` for the zero timestamp |
| time of day | `"hh:mm:ss"`; `null` for "no time" |
| lines | array of strings |
| bytes | hex string; `null` for "no value" |
| identifier (`isin` of the stock instrument) | `{"text", "kind", "is_null", "isin", "isin_fails"}`: the text the application holds, the kind (0 none, 1 ISIN, 2 national number, 3 ticker symbol, 4 bilateral), whether it is "no identifier", the canonical ISIN Banking4 derives, and whether that derivation fails |
| splits | array of `{"date", "reverse", "factor"}`; `null` for no list |

## Things a test should know

- The `title` of an account document is replaced by "no value" on reading when
  it equals a title the application generates from the other fields in its
  user interface language. The four records whose `about` starts with "title
  equal to the generated one" show this: `written.title` is the German
  generated title, `read.title` is `null`. A decoder cannot reproduce that
  and should keep the title as stored (section 3.12.2).
- The `read` values of category names are normalised with the German user
  interface texts (section 4.2).
- The canonical `rules` text of a category is sorted by a language-dependent
  comparison (section 4.5). Compare rule sets, not the text, unless all
  keywords consist of the letters a to z.
- No record has an update stamp of -1: Banking4 replaces that by the current
  time on writing, so such a record would not have fixed bytes.

## Regenerating

Nothing in this repository builds these files. Do not edit them: each `.json`
describes exactly the bytes of its `.bin`.

`category.bin` contains rule texts in the order of a German string comparison
(section 4.5). That order depends on collation data, and other collation data
could order a few of them differently.
