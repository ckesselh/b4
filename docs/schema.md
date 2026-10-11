# The database

This is the layout of the DuckDB database that `b4 decrypt` writes: every
table, view and column with its type and description. The same texts are
stored in the database and shown by `b4 schema`.

This file is generated from `src/db/schema.rs`; do not edit it. Regenerate it
with `task docs`.

Conventions, the same in every table:

- `id` is the record's object id as stored in the vault. Rows of per-account
  tables carry `account_id` as well. Nothing is declared unique: the format
  guarantees no uniqueness, so no table has a key or a constraint.
- Money is `DECIMAL(18,4)`; quantities, prices and rates are `DECIMAL(28,10)`.
  Sums are exact.
- Amounts are signed: negative is money leaving the account.
- A coded value is its lower-case name in a `VARCHAR`; a code this version does
  not know is its number as text. A flag set is the names of the set flags
  joined by commas.
- An empty text is NULL.
- Timestamps have no time zone unless a description says UTC.

## Tables and views

### `accounts`

One row per account of the vault: bank accounts, cash, credit cards, securities accounts and others, including hidden ones (see `hidden`). Bank access data of an account is never exported.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the account. Other tables refer to it as account_id. Not guaranteed unique. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `kind` | `VARCHAR` | Kind of account: bank, cash, credit_card, portfolio, asset, paypal or crypto. |
| `stored_kind` | `VARCHAR` | The kind as stored. Differs from `kind` only for a cash account in a crypto currency, which Banking4 treats as crypto. |
| `name` | `VARCHAR` | Name the user gave the account. |
| `iban` | `VARCHAR` | IBAN, without blanks. NULL for accounts that have none. |
| `bic` | `VARCHAR` | BIC of the account's bank. |
| `bank_code` | `VARCHAR` | National bank code. Accounts that are not at a bank carry a pseudo code. |
| `account_number` | `VARCHAR` | National account number. |
| `country_code` | `VARCHAR` | Country code of the bank. |
| `currency` | `VARCHAR` | Currency of the account as an ISO code. NULL for accounts without a currency of their own, such as securities accounts. |
| `holder_name` | `VARCHAR` | Name of the account holder. |
| `holder_street` | `VARCHAR` | Street of the holder's address. |
| `holder_town` | `VARCHAR` | Postal code and town of the holder's address. |
| `description` | `VARCHAR` | Free text the user entered. |
| `group_id` | `BIGINT` | The group the account is sorted into (account_groups.id). NULL when in no group. |
| `position` | `BIGINT` | Sort position in Banking4: order by position, then id. |
| `opening_date` | `DATE` | Day the account was opened. |
| `balance` | `DECIMAL(18,4)` | Balance according to the last statement, in `currency`; negative when overdrawn. Zero for kinds without statements (portfolio, asset). As stored: not computed from bookings (see the view account_balances). |
| `balance_at` | `TIMESTAMP` | Moment `balance` refers to; zone unknown, often midnight of the statement day. NULL when there is no statement balance. |
| `online_balance` | `DECIMAL(18,4)` | Balance the bank reported in the last online query, which can include entries that are not booked yet. |
| `online_balance_at` | `TIMESTAMP` | Moment `online_balance` was reported; zone unknown. NULL when never queried. |
| `available_funds` | `DECIMAL(18,4)` | Amount available according to the bank or computed by Banking4. |
| `overdraft_limit` | `DECIMAL(18,4)` | Overdraft limit. |
| `credit_interest_rate` | `DECIMAL(28,10)` | Interest rate on a credit balance, in percent. |
| `debit_interest_rate` | `DECIMAL(28,10)` | Interest rate on an overdraft within the limit, in percent. |
| `overdraft_interest_rate` | `DECIMAL(28,10)` | Interest rate beyond the overdraft limit, in percent. |
| `last_download_at` | `TIMESTAMP` | When statements were last fetched; zone unknown. NULL when never. |
| `last_sync_at` | `TIMESTAMP` | When the account was last synchronised; zone unknown. NULL when never. |
| `flags` | `VARCHAR` | Settings of the account as a comma-joined list of flag names, such as `hidden,exclude_from_totals`. NULL when none is set. |
| `hidden` | `BOOLEAN` | True when the user hid the account in Banking4. |
| `exclude_from_totals` | `BOOLEAN` | True when the account is left out of Banking4's total of all balances. |
| `balance_flags` | `VARCHAR` | Where limit and available funds come from and what is displayed, as flag names. |
| `creditor_id` | `VARCHAR` | SEPA creditor identifier used for direct debits from this account. |
| `reference_account_iban` | `VARCHAR` | IBAN of the reference account, for savings and call money accounts. |
| `originator_name` | `VARCHAR` | Name used as originator in payments from this account. |
| `icon_name` | `VARCHAR` | Name of the icon chosen for the account. |
| `record_version` | `BIGINT` | Layout version of the stored record; tells which generation of Banking4 wrote it. |

### `account_groups`

One row per group that accounts are sorted into.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the group; accounts.group_id refers to it. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `position` | `BIGINT` | Sort position in Banking4. |
| `name` | `VARCHAR` | Name of the group. |
| `description` | `VARCHAR` | Free text the user entered. |
| `flags` | `VARCHAR` | Settings of the group as flag names. |
| `exclude_from_totals` | `BOOLEAN` | True when the group is left out of Banking4's total of all balances. |

### `bookings`

One row per statement entry of an account, booked or pending. `amount` is signed (negative = money leaving the account). The master row of a split or batch and its parts are separate rows: sum only rows where `counted` is true. The view bookings_view adds account and category context.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `booking_date` | `DATE` | Day the bank booked the entry. NULL for an entry that is not booked yet and for a stored date that is no calendar date. |
| `value_date` | `DATE` | Value date. NULL when the bank gave none, and always for cash accounts. |
| `status` | `VARCHAR` | booked, pending (announced by the bank, not booked yet) or information (a notice that moves no money). |
| `pending` | `BOOLEAN` | True when status is not booked. Pending entries are not part of the account's stored balance. |
| `amount` | `DECIMAL(18,4)` | Signed amount in `currency`: negative when money leaves the account, positive when it arrives. Rounded to four decimals if the stored value has more. |
| `debit` | `BOOLEAN` | Direction: true when money leaves the account (amount is negative or zero), false when it arrives. |
| `stored_amount` | `DECIMAL(28,10)` | The amount as the vault stores it, without the direction; normally not negative. |
| `currency` | `VARCHAR` | Currency of `amount`: the account's currency, which is what Banking4 shows, in a PayPal account too. The stored code for a booking whose account has no currency or no row in `accounts`. |
| `stored_currency` | `VARCHAR` | The currency code stored in the record. Banking4 keeps it only in PayPal accounts, where it can differ from `currency`; elsewhere it replaces it with the account's. |
| `payee_name` | `VARCHAR` | Name of the other party: the recipient of a debit, the sender of a credit. |
| `payee_iban` | `VARCHAR` | Account of the other party: an IBAN, or a national account number in old records. |
| `payee_bic` | `VARCHAR` | Bank of the other party: a BIC, or a national bank code in old records. |
| `payee_country` | `VARCHAR` | Country code of the other party. |
| `payee_street` | `VARCHAR` | Street of the other party's address. |
| `payee_town` | `VARCHAR` | Postal code and town of the other party's address. |
| `ultimate_payee_name` | `VARCHAR` | Name of the party on whose behalf the other party acts (SEPA ultimate creditor or debtor). |
| `purpose` | `VARCHAR` | Remittance text; several lines are joined with a line feed. |
| `category` | `VARCHAR` | Category as the record names it: the full name with levels separated by `:`, such as `Wohnen:Miete`. NULL when uncategorised. Need not exist in `categories`. |
| `category_key` | `VARCHAR` | Matching key of `category`: the name trimmed and upper-cased. Join it to categories.category_key; names that differ only in case are one category. |
| `booking_text` | `VARCHAR` | The bank's name for the kind of transaction, such as `Lastschrift`. |
| `notes` | `VARCHAR` | Note the user attached to the entry. |
| `counted` | `BOOLEAN` | False for the master row of a split or batch that has parts: its parts carry the money. Sum `amount` over rows where counted is true, or money is counted twice. |
| `split_role` | `VARCHAR` | Role in a split or batch: split_master, split_slave, batch_master or batch_slave. NULL for an ordinary entry. |
| `master_id` | `BIGINT` | id of the master row of the split or batch the entry belongs to, in the same account. A master points at itself. NULL for an ordinary entry. |
| `batch_index` | `BIGINT` | Position within its split or batch: 0 for the master and for ordinary entries, 1 and up for the parts. |
| `flag` | `VARCHAR` | Colour mark the user set: none, ok, red, blue, yellow, green, gray or purple. |
| `is_read` | `BOOLEAN` | True when the user has seen the entry. |
| `entry_bits` | `VARCHAR` | Processing marks as flag names: read, exported_to_books, uploaded. |
| `transaction_code` | `VARCHAR` | Kind of transaction as stored: other, remittance, debit, withdrawal_or_deposit, check, standing_order, reversal, charges, cash_management or card_payment. |
| `gvc` | `BIGINT` | The German banks' business transaction code (Geschäftsvorfallcode); 0 when none. |
| `gvc_extension` | `BIGINT` | Extension of the business transaction code. |
| `swift_transaction_code` | `VARCHAR` | SWIFT transaction type of the statement line, such as NTRF. |
| `primanota` | `VARCHAR` | The bank's journal number. |
| `bank_reference` | `VARCHAR` | The bank's reference for the entry. |
| `end_to_end_id` | `VARCHAR` | SEPA end-to-end reference. |
| `payment_info_id` | `VARCHAR` | SEPA payment information id, shared by the transfers of one batch. |
| `mandate_id` | `VARCHAR` | Mandate reference of a direct debit. |
| `mandate_signed_date` | `DATE` | Day the direct debit mandate was signed. |
| `creditor_id` | `VARCHAR` | SEPA creditor identifier of a direct debit. |
| `purpose_code` | `VARCHAR` | SEPA purpose code, such as SALA. |
| `category_purpose_code` | `VARCHAR` | SEPA category purpose code. |
| `local_instrument_code` | `VARCHAR` | SEPA local instrument code, such as CORE or B2B. |
| `sequence_type` | `VARCHAR` | SEPA direct debit sequence type, such as FRST or RCUR. |
| `order_id` | `VARCHAR` | Order number the bank assigned. |
| `transaction_date` | `DATE` | Day the transaction took place, for card payments. NULL when not given, and always for cash accounts. |
| `analysis_date` | `DATE` | Day the user assigned the entry to for evaluations, when it differs from the booking date. NULL when not set. |
| `vat_amount` | `DECIMAL(18,4)` | Value added tax contained in the amount, without sign; 0 when none. |
| `vat_rate_type` | `VARCHAR` | Kind of tax rate as flag names. |
| `instructed_amount` | `DECIMAL(18,4)` | Original amount in `instructed_currency` when the transaction was in another currency; 0 when none. Sign as delivered by the bank. |
| `instructed_currency` | `VARCHAR` | Currency of `instructed_amount`. |
| `charges_amount` | `DECIMAL(18,4)` | Charges contained in the entry, in `charges_currency`; 0 when none. |
| `charges_currency` | `VARCHAR` | Currency of `charges_amount`. |
| `virtual_iban` | `VARCHAR` | Virtual IBAN the payment was addressed to. |
| `debit_return_order_id` | `VARCHAR` | Order number of a returned direct debit. |
| `debit_return_date` | `DATE` | Day a direct debit was returned. |
| `debit_return_time` | `TIME` | Time of day a direct debit was returned. |
| `cost_bearer` | `VARCHAR` | Who pays the charges of a foreign payment: none, ours, shared or beneficiary. |
| `intl_remittance_method` | `VARCHAR` | Remittance method of a foreign payment. |
| `intl_remittance_instruction` | `VARCHAR` | Instruction text of a foreign payment. |
| `regulatory_reporting_code` | `VARCHAR` | Regulatory reporting code of a foreign payment. |
| `payee_bank_name` | `VARCHAR` | Name of the other party's bank. |
| `payee_bank_street` | `VARCHAR` | Street of the other party's bank. |
| `payee_bank_town` | `VARCHAR` | Town of the other party's bank. |

### `payments`

One row per payment entered in Banking4 (transfers and direct debits), sent or not. `amount` is signed like in bookings. These are orders, not account movements: what the bank booked is in `bookings`. TANs are never exported.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `scheme` | `VARCHAR` | Kind of payment: none, user_expense, user_income, sepa_credit_transfer, sepa_direct_debit_core, sepa_direct_debit_b2b, sepa_instant_credit_transfer, urgent_credit_transfer, foreign_credit_transfer, swiss_credit_transfer, domestic_legacy_credit_transfer or foreign_legacy_credit_transfer. Derived from the stored class when an old record stores none. |
| `stored_scheme` | `VARCHAR` | The scheme as stored: none in records of the old layout, where `scheme` is derived. |
| `amount` | `DECIMAL(18,4)` | Signed amount in `currency`: negative when money leaves the account, positive when it arrives. Rounded to four decimals if the stored value has more. |
| `debit` | `BOOLEAN` | Direction: true when money leaves the account (amount is negative or zero), false when it arrives. |
| `stored_amount` | `DECIMAL(28,10)` | The amount as the vault stores it, without the direction; normally not negative. |
| `stored_debit_flag` | `BOOLEAN` | The direction bit as stored. `debit` is what counts: for a record with a scheme the scheme decides the direction and this bit can differ. |
| `currency` | `VARCHAR` | Currency of `amount`: the stored code, or the scheme's or the account's currency when none is stored. |
| `stored_currency` | `VARCHAR` | The currency code stored in the record. |
| `scheduled_date` | `DATE` | Day the payment is to be executed. NULL for immediate execution. |
| `scheduled_time` | `TIME` | Time of day for the execution; only meaningful for instant transfers. |
| `payee_name` | `VARCHAR` | Name of the other party: the recipient of a debit, the sender of a credit. |
| `payee_iban` | `VARCHAR` | Account of the other party: an IBAN, or a national account number in old records. |
| `payee_bic` | `VARCHAR` | Bank of the other party: a BIC, or a national bank code in old records. |
| `payee_country` | `VARCHAR` | Country code of the other party. |
| `payee_street` | `VARCHAR` | Street of the other party's address. |
| `payee_town` | `VARCHAR` | Postal code and town of the other party's address. |
| `ultimate_payee_name` | `VARCHAR` | Name of the party on whose behalf the other party acts (SEPA ultimate creditor or debtor). |
| `purpose` | `VARCHAR` | Remittance text; several lines are joined with a line feed. |
| `category` | `VARCHAR` | Category as the record names it: the full name with levels separated by `:`, such as `Wohnen:Miete`. NULL when uncategorised. Need not exist in `categories`. |
| `category_key` | `VARCHAR` | Matching key of `category`: the name trimmed and upper-cased. Join it to categories.category_key; names that differ only in case are one category. |
| `end_to_end_id` | `VARCHAR` | SEPA end-to-end reference. |
| `payment_info_id` | `VARCHAR` | SEPA payment information id, shared by the transfers of one batch. |
| `mandate_id` | `VARCHAR` | Mandate reference of a direct debit. |
| `mandate_signed_date` | `DATE` | Day the direct debit mandate was signed. |
| `creditor_id` | `VARCHAR` | SEPA creditor identifier of a direct debit. |
| `purpose_code` | `VARCHAR` | SEPA purpose code, such as SALA. |
| `category_purpose_code` | `VARCHAR` | SEPA category purpose code. |
| `local_instrument_code` | `VARCHAR` | SEPA local instrument code, such as CORE or B2B. |
| `sequence_type` | `VARCHAR` | SEPA direct debit sequence type, such as FRST or RCUR. |
| `order_id` | `VARCHAR` | Order number the bank assigned. |
| `state` | `VARCHAR` | Where the payment stands, as flag names: local (entered, not sent), in_outbox, sent, postdated_at_bank, reconciled (matched with a booking) and others. |
| `flags` | `VARCHAR` | Further marks of the payment as flag names. |
| `hidden_by_banking4` | `BOOLEAN` | True for a payment Banking4 itself drops when it loads the file. It is kept here because the vault holds it. |
| `master_id` | `BIGINT` | id of the master row of the batch the payment belongs to, in the same account. A master points at itself. NULL when not in a batch. |
| `batch_index` | `BIGINT` | Position within its batch: 0 for the master and for single payments. |
| `flag` | `VARCHAR` | Colour mark the user set, as in bookings. |
| `initiator_name` | `VARCHAR` | Name of the person who entered the payment. |
| `batch_booking` | `VARCHAR` | Whether a batch is to be booked as one entry: default, yes, no or mixed. |
| `import_id` | `VARCHAR` | Identifier from the file the payment was imported from. |
| `payment_reference` | `VARCHAR` | Reference Banking4 gave the payment. |
| `reconciled_booking_id` | `BIGINT` | id of the booking the payment was matched with (bookings.id of the same account). NULL when not matched. |
| `outbox_id` | `BIGINT` | Object id of the outbox order that carries the payment. The outbox itself is bank access data and is not exported. NULL when none. |
| `sepa_status` | `VARCHAR` | Status the bank reported, as a name: none, received, pending, rejected, the accepted_* stages, or unknown for a code b4 does not know. |
| `sepa_status_code` | `VARCHAR` | The four-letter status code as stored, such as ACSC. |
| `sepa_reason_code` | `VARCHAR` | Reason code the bank gave with the status. |
| `sepa_additional_info` | `VARCHAR` | Additional status text from the bank. |
| `payee_verification_choice` | `VARCHAR` | The user's choice for the verification of the payee: default, opt_in or opt_out. |
| `payee_verification_json` | `VARCHAR` | Result of the bank's verification of the payee, as the stored JSON text. |
| `cost_bearer_account_id` | `BIGINT` | Account that bears the charges of a foreign payment (accounts.id). NULL when none. |
| `cost_bearer` | `VARCHAR` | Who pays the charges of a foreign payment: none, ours, shared or beneficiary. |
| `intl_remittance_method` | `VARCHAR` | Remittance method of a foreign payment. |
| `intl_remittance_instruction` | `VARCHAR` | Instruction text of a foreign payment. |
| `regulatory_reporting_code` | `VARCHAR` | Regulatory reporting code of a foreign payment. |
| `payee_bank_name` | `VARCHAR` | Name of the other party's bank. |
| `payee_bank_street` | `VARCHAR` | Street of the other party's bank. |
| `payee_bank_town` | `VARCHAR` | Town of the other party's bank. |

### `standing_orders`

One row per standing order of an account, held at the bank or kept in Banking4 only. `amount` is signed like in bookings and is the amount of one execution.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `scheme` | `VARCHAR` | Kind of payment: none, user_expense, user_income, sepa_credit_transfer, sepa_direct_debit_core, sepa_direct_debit_b2b, sepa_instant_credit_transfer, urgent_credit_transfer, foreign_credit_transfer, swiss_credit_transfer, domestic_legacy_credit_transfer or foreign_legacy_credit_transfer. Derived from the stored class when an old record stores none. |
| `stored_scheme` | `VARCHAR` | The scheme as stored: none in records of the old layout, where `scheme` is derived. |
| `amount` | `DECIMAL(18,4)` | Signed amount in `currency`: negative when money leaves the account, positive when it arrives. Rounded to four decimals if the stored value has more. |
| `debit` | `BOOLEAN` | Direction: true when money leaves the account (amount is negative or zero), false when it arrives. |
| `stored_amount` | `DECIMAL(28,10)` | The amount as the vault stores it, without the direction; normally not negative. |
| `stored_debit_flag` | `BOOLEAN` | The direction bit as stored. `debit` is what counts: for a record with a scheme the scheme decides the direction and this bit can differ. |
| `currency` | `VARCHAR` | Currency of `amount`: the stored code, or the scheme's or the account's currency when none is stored. |
| `stored_currency` | `VARCHAR` | The currency code stored in the record. |
| `payee_name` | `VARCHAR` | Name of the other party: the recipient of a debit, the sender of a credit. |
| `payee_iban` | `VARCHAR` | Account of the other party: an IBAN, or a national account number in old records. |
| `payee_bic` | `VARCHAR` | Bank of the other party: a BIC, or a national bank code in old records. |
| `payee_country` | `VARCHAR` | Country code of the other party. |
| `payee_street` | `VARCHAR` | Street of the other party's address. |
| `payee_town` | `VARCHAR` | Postal code and town of the other party's address. |
| `ultimate_payee_name` | `VARCHAR` | Name of the party on whose behalf the other party acts (SEPA ultimate creditor or debtor). |
| `purpose` | `VARCHAR` | Remittance text; several lines are joined with a line feed. |
| `category` | `VARCHAR` | Category as the record names it: the full name with levels separated by `:`, such as `Wohnen:Miete`. NULL when uncategorised. Need not exist in `categories`. |
| `category_key` | `VARCHAR` | Matching key of `category`: the name trimmed and upper-cased. Join it to categories.category_key; names that differ only in case are one category. |
| `end_to_end_id` | `VARCHAR` | SEPA end-to-end reference. |
| `payment_info_id` | `VARCHAR` | SEPA payment information id, shared by the transfers of one batch. |
| `mandate_id` | `VARCHAR` | Mandate reference of a direct debit. |
| `mandate_signed_date` | `DATE` | Day the direct debit mandate was signed. |
| `creditor_id` | `VARCHAR` | SEPA creditor identifier of a direct debit. |
| `purpose_code` | `VARCHAR` | SEPA purpose code, such as SALA. |
| `category_purpose_code` | `VARCHAR` | SEPA category purpose code. |
| `local_instrument_code` | `VARCHAR` | SEPA local instrument code, such as CORE or B2B. |
| `sequence_type` | `VARCHAR` | SEPA direct debit sequence type, such as FRST or RCUR. |
| `order_id` | `VARCHAR` | Order number the bank assigned. |
| `state` | `VARCHAR` | Where the order stands, as flag names: at_bank, local (kept in Banking4 only), setup_in_outbox, change_in_outbox, cancel_in_outbox, auto_detected, deleted. |
| `is_local` | `BOOLEAN` | True when the order is kept in Banking4 only and the bank does not know it. |
| `first_execution_date` | `DATE` | Day of the first execution. |
| `last_execution_date` | `DATE` | Day of the last execution. NULL means until further notice. |
| `next_execution_date` | `DATE` | Next execution day, for local orders only. NULL for orders held at the bank: Banking4 computes their next day and b4 does not. |
| `stored_next_execution_date` | `DATE` | The next execution day as stored; can be out of date for orders held at the bank. |
| `period_unit` | `VARCHAR` | Unit of the interval: month or week. |
| `period_length` | `BIGINT` | Length of the interval in units: 1 with month is monthly, 3 quarterly. |
| `execution_day` | `BIGINT` | Day of the month or of the week the order runs on; 97 to 99 stand for days counted from the end of the month. |
| `execution_time` | `TIME` | Time of day for the execution. |
| `outbox_id` | `BIGINT` | Object id of the outbox order that carries a change to the standing order. NULL when none. |
| `cost_bearer` | `VARCHAR` | Who pays the charges of a foreign payment: none, ours, shared or beneficiary. |
| `intl_remittance_method` | `VARCHAR` | Remittance method of a foreign payment. |
| `intl_remittance_instruction` | `VARCHAR` | Instruction text of a foreign payment. |
| `regulatory_reporting_code` | `VARCHAR` | Regulatory reporting code of a foreign payment. |
| `payee_bank_name` | `VARCHAR` | Name of the other party's bank. |
| `payee_bank_street` | `VARCHAR` | Street of the other party's bank. |
| `payee_bank_town` | `VARCHAR` | Town of the other party's bank. |

### `payees`

One row per entry of the address book of counterparties. Bookings do not refer to payees by id: they carry the name and account themselves.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the payee; payers.payee_id refers to it. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `name` | `VARCHAR` | Name of the payee. |
| `iban` | `VARCHAR` | IBAN. Several payees can share one. |
| `bic` | `VARCHAR` | BIC of the payee's bank. |
| `account_number` | `VARCHAR` | National account number, in old entries. |
| `bank_code` | `VARCHAR` | National bank code, in old entries. |
| `street` | `VARCHAR` | Street of the payee's address. |
| `town` | `VARCHAR` | Postal code and town of the payee's address. |
| `country` | `VARCHAR` | Country code of the payee. |
| `bank_name` | `VARCHAR` | Name of the payee's bank. |
| `bank_street` | `VARCHAR` | Street of the payee's bank. |
| `bank_town` | `VARCHAR` | Town of the payee's bank. |
| `purpose` | `VARCHAR` | Default remittance text for payments to the payee, as stored. |
| `category` | `VARCHAR` | Default category for payments to the payee, as a full name. |
| `category_key` | `VARCHAR` | Matching key of `category`; join it to categories.category_key. |
| `amount` | `DECIMAL(18,4)` | Default amount for payments to the payee, without direction; 0 when none. |
| `currency` | `VARCHAR` | Currency of the default amount. |
| `email` | `VARCHAR` | E-mail address. |
| `customer_number` | `VARCHAR` | The user's customer number at the payee. |
| `notes` | `VARCHAR` | Notes the user entered, with their line breaks as stored. |
| `kind` | `VARCHAR` | none, payee, legacy_mandate (a payee with an old-style mandate) or auto (created by Banking4 from a booking). |
| `stored_kind` | `VARCHAR` | The kind as stored, before Banking4 reduces it. |
| `flags` | `VARCHAR` | Marks of the payee as flag names. |
| `vop_status` | `VARCHAR` | Result of the last verification of the payee's name by the bank: none, match, close_match, no_match, not_applicable or pending. |
| `vop_matched_name` | `VARCHAR` | The name the bank reported for a close match. |
| `mandate_id` | `VARCHAR` | Mandate reference of an old-style mandate stored with the payee. |
| `mandate_signed_date` | `DATE` | Day that mandate was signed. |
| `mandate_revocation_date` | `DATE` | Day that mandate was revoked. |
| `local_instrument_code` | `VARCHAR` | SEPA local instrument code of that mandate. |

### `payers`

One row per counterparty the user collects direct debits from, with the mandate that allows it.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the payer. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `payee_id` | `BIGINT` | The address book entry the payer belongs to (payees.id). |
| `name` | `VARCHAR` | Name of the payer. |
| `mandate_id` | `VARCHAR` | Mandate reference. |
| `mandate_signed_date` | `DATE` | Day the mandate was signed. |
| `mandate_revocation_date` | `DATE` | Day the mandate was revoked. |
| `local_instrument_code` | `VARCHAR` | SEPA local instrument code, such as CORE or B2B. |
| `notes` | `VARCHAR` | Notes the user entered, with their line breaks as stored. |
| `category` | `VARCHAR` | Default category for direct debits from the payer, as a full name. |
| `category_key` | `VARCHAR` | Matching key of `category`; join it to categories.category_key. |

### `categories`

One row per category of the catalogue, a tree whose levels are separated by `:` in the name. Bookings name their category by text and can use names that have no row here.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the category. Nothing refers to it: other records name a category by its text. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `full_name` | `VARCHAR` | Full name with levels separated by `:`, such as `Wohnen:Nebenkosten:Strom`. NULL for a record without a usable name. |
| `category_key` | `VARCHAR` | Matching key: full_name trimmed and upper-cased. bookings.category_key joins here. Not guaranteed unique. |
| `name` | `VARCHAR` | The last level of full_name. |
| `parent` | `VARCHAR` | Full name of the parent category. NULL for a top-level category. The parent need not exist as a row. |
| `parent_key` | `VARCHAR` | category_key of the parent. NULL for a top-level category. |
| `depth` | `BIGINT` | Level in the tree: 0 for a top-level category. |
| `description` | `VARCHAR` | Free text the user entered. |
| `special` | `VARCHAR` | none for an ordinary category; reclassification (transfers between own accounts), excluded (left out of evaluations) or vat for Banking4's built-in categories. The role the category has in Banking4: each role belongs to the first category that stores it, a later one that stores it again is an ordinary category here, and so is one Banking4 drops. See stored_special. |
| `totals_bucket` | `VARCHAR` | How Banking4 counts the category in evaluations: auto (by the sign of each booking), income, expense or exclude. |
| `budget_period` | `VARCHAR` | Period the budget applies to: none, week, month, quarter, year, all or month_range. |
| `budget_amount` | `DECIMAL(18,4)` | Budget per `budget_period`, as stored; 0 when none. |
| `vat_percent` | `DECIMAL(28,10)` | Value added tax rate of the category in percent; 0 when none. |
| `icon_name` | `VARCHAR` | Name of the icon. |
| `color_name` | `VARCHAR` | Name of the colour. |
| `rules` | `VARCHAR` | Keyword rules that assign bookings to the category automatically, as the stored expression. |
| `layout_version` | `BIGINT` | Layout version of the stored record; needed to interpret `rules`, whose syntax changed with version 4. |
| `stored_full_name` | `VARCHAR` | The name as stored, before normalisation. |
| `hidden_by_banking4` | `BOOLEAN` | True for a record Banking4 itself drops when it loads the catalogue. |
| `stored_special` | `VARCHAR` | The special role as stored in the record. Differs from `special` for a category that repeats the role of an earlier one and for one Banking4 drops. |

### `mandates`

One row per direct debit mandate the user gave to a creditor, as Banking4 collected them from bookings.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the mandate. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `creditor_id` | `VARCHAR` | SEPA creditor identifier of the creditor. |
| `mandate_id` | `VARCHAR` | Mandate reference. |
| `creditor_name` | `VARCHAR` | Name of the creditor. |
| `category` | `VARCHAR` | Category for direct debits under the mandate, as a full name. |
| `category_key` | `VARCHAR` | Matching key of `category`; join it to categories.category_key. |
| `flags` | `VARCHAR` | Marks of the mandate as flag names. |
| `hidden` | `BOOLEAN` | True when the user hid the mandate. |
| `revoked` | `BOOLEAN` | True when the mandate was revoked. |
| `mandate_date` | `TIMESTAMP` | Date of the mandate; zone unknown, normally midnight. |
| `hidden_by_banking4` | `BOOLEAN` | True for a record Banking4 itself drops when it loads the file. |

### `exchange_rates`

One row per stored exchange rate, the latest quote per currency pair. Stored as found: no rate is inverted or converted.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the rate. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `base_currency` | `VARCHAR` | Currency one unit of which is priced. |
| `target_currency` | `VARCHAR` | Currency the price is given in. |
| `rate` | `DECIMAL(28,10)` | Units of target_currency per one unit of base_currency, as stored. |
| `quoted_at` | `TIMESTAMP` | When the rate was quoted; zone unknown. |
| `hidden_by_banking4` | `BOOLEAN` | True for a rate Banking4 itself drops when it loads the file (a duplicate or one it cannot use). |

### `stock_instruments`

One row per security Banking4 keeps prices for.

| Column | Type | Description |
|---|---|---|
| `id` | `BIGINT` | Object id of the instrument; stock_prices.instrument_id refers to it. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `isin` | `VARCHAR` | ISIN of the security. NULL when the stored identifier is no ISIN and yields none. portfolio_items.isin joins here. |
| `stored_isin` | `VARCHAR` | The identifier text as stored. |
| `name` | `VARCHAR` | Name of the security. |
| `price_currency` | `VARCHAR` | Currency the prices of the instrument are in. |
| `splits` | `VARCHAR` | Stock splits as the stored text, one per line. NULL when none. No price is adjusted for a split. |

### `stock_prices`

One row per stored daily price of a security. More than one row per day is possible.

| Column | Type | Description |
|---|---|---|
| `instrument_id` | `BIGINT` | The instrument the price belongs to (stock_instruments.id), taken from the directory the record is stored in. |
| `id` | `BIGINT` | Object id of the price record. Unique within one instrument at most. |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `price_date` | `DATE` | Day the price is for. |
| `price` | `DECIMAL(28,10)` | Price of one unit in the instrument's price_currency. |

### `portfolio_items`

One row per position of a securities account. No market value is stored: for a position in units it is quantity times current_price.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `state` | `VARCHAR` | Origin and state as flag names: bank (reported by the bank), local (entered by hand), deleted. |
| `deleted` | `BOOLEAN` | True when the position is marked deleted. |
| `isin` | `VARCHAR` | ISIN of the security; joins to stock_instruments.isin. NULL when the stored identifier yields none. |
| `stored_isin` | `VARCHAR` | The identifier text as stored. |
| `name` | `VARCHAR` | Name of the security. |
| `quantity` | `DECIMAL(28,10)` | Size of the position: a number of units, or a face amount in quantity_currency (see quantity_type). |
| `quantity_type` | `VARCHAR` | units (pieces), face_amount (nominal value, for bonds) or undefined. |
| `quantity_currency` | `VARCHAR` | Currency of a face amount. NULL when the quantity counts units. |
| `price_type` | `VARCHAR` | How prices are quoted: actual (money per unit), percent (of the face amount), yield or undefined. |
| `current_price` | `DECIMAL(28,10)` | Latest price, in current_price_currency or in percent; never negative. |
| `current_price_currency` | `VARCHAR` | Currency of current_price. |
| `current_price_date` | `DATE` | Day of the latest price. |
| `current_price_time` | `TIME` | Time of day of the latest price. |
| `purchase_price` | `DECIMAL(28,10)` | Purchase price per unit, in purchase_price_currency or in percent; never negative. |
| `purchase_price_currency` | `VARCHAR` | Currency of purchase_price. |
| `purchase_price_date` | `DATE` | Day of the purchase. |
| `interest_rate` | `DECIMAL(28,10)` | Interest rate of a bond in percent; 0 when none. |
| `maturity_date` | `DATE` | Day a bond matures. |
| `blocked_until_date` | `DATE` | Day until which the position is blocked. |
| `alert_lower_limit` | `DECIMAL(28,10)` | Price below which the user wants to be alerted; 0 when none. |
| `alert_upper_limit` | `DECIMAL(28,10)` | Price above which the user wants to be alerted; 0 when none. |
| `portfolio_key` | `VARCHAR` | Key of the position at the bank. |
| `additional_info` | `VARCHAR` | Further text for the position; several lines are joined with a line feed. |

### `portfolio_transactions`

One row per securities transaction of a securities account (purchases, sales, corporate actions).

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `transaction_id` | `VARCHAR` | The bank's identifier of the transaction. |
| `state` | `VARCHAR` | none, pending, executed or settled. |
| `delivery_direction` | `VARCHAR` | Which way the securities moved: receive (a purchase), deliver (a sale) or none. |
| `activity` | `VARCHAR` | Kind of transaction: settlement, corporate_action, collateral, borrowing_lending or none. |
| `isin` | `VARCHAR` | ISIN of the security; joins to stock_instruments.isin. |
| `stored_isin` | `VARCHAR` | The identifier text as stored. |
| `name` | `VARCHAR` | Name of the security. |
| `quantity` | `DECIMAL(28,10)` | Number of units or face amount moved, without sign (see delivery_direction). |
| `quantity_type` | `VARCHAR` | units, face_amount or undefined. |
| `quantity_currency` | `VARCHAR` | Currency of a face amount. |
| `traded_at` | `TIMESTAMP` | When the trade took place; zone unknown. |
| `trading_price` | `DECIMAL(28,10)` | Price per unit of the trade, in trading_currency. |
| `trading_currency` | `VARCHAR` | Currency of trading_price. |
| `settled_at` | `TIMESTAMP` | Settlement day; zone unknown, normally midnight. |
| `settlement_amount` | `DECIMAL(18,4)` | Money that changed hands, without sign (see delivery_direction). |
| `settlement_currency` | `VARCHAR` | Currency of settlement_amount. |
| `interest_accrued` | `BOOLEAN` | True when accrued interest was settled with the trade. |
| `interest_days` | `BIGINT` | Number of days of accrued interest. |
| `interest_amount` | `DECIMAL(18,4)` | Accrued interest; 0 when none. |
| `interest_currency` | `VARCHAR` | Currency of interest_amount. |
| `is_read` | `BOOLEAN` | True when the user has seen the transaction. |

### `tax_exemptions`

One row per tax exemption order (Freistellungsauftrag) of an account.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `valid_from` | `TIMESTAMP` | First day the order is valid; zone unknown, normally midnight. |
| `valid_until` | `TIMESTAMP` | Last day the order is valid; zone unknown. |
| `amount` | `DECIMAL(18,4)` | Exempted amount per year. |
| `used_amount` | `DECIMAL(18,4)` | Part of it used so far, as stored. |
| `remaining_amount` | `DECIMAL(18,4)` | Part of it still available, as stored. |

### `assets`

One row per item of an asset account: term deposits, savings plans, loans.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `asset_id` | `VARCHAR` | Identifier of the item at the bank. |
| `amount` | `DECIMAL(18,4)` | Signed value of the item in `currency`: negative for a liability such as a loan. |
| `debit` | `BOOLEAN` | True when the item is a liability. |
| `currency` | `VARCHAR` | Currency of `amount`. |
| `deposit_date` | `DATE` | Day the money was deposited or the loan paid out. |
| `expiry_date` | `DATE` | Day the item ends. |
| `interest_rate` | `DECIMAL(28,10)` | Interest rate in percent. |
| `interest_method` | `VARCHAR` | Code of the interest calculation method. |
| `description` | `VARCHAR` | Description of the item. |

### `meta`

Describes the database itself, one row per key: the b4 and schema version that wrote it, the vault it came from and when it was decrypted.

| Column | Type | Description |
|---|---|---|
| `key` | `VARCHAR` | Name of the entry, such as schema_version, vault_path or decrypted_at. |
| `value` | `VARCHAR` | Its value as text. |

### `warnings`

What the reader noticed while decrypting, one row per warning: vault entries it does not know, records it could not decode, values that did not fit their column. A warning names files, columns and counts, never a value from the vault.

| Column | Type | Description |
|---|---|---|
| `message` | `VARCHAR` | The warning as one sentence. |

### `bookings_view` (view)

Every row of `bookings` with all its columns, plus the name, IBAN, kind and currency of its account and the levels of its category. Nothing is filtered out: filter on `counted` (and usually `not pending`) before summing amounts.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account the record belongs to (accounts.id), taken from the directory the record is stored in. Normally an account exists for it; records of a directory without an account are kept with a warning. |
| `id` | `BIGINT` | Object id of the record. Unique within one account at most: identify a row by (account_id, id). |
| `created_at` | `TIMESTAMP` | When the record was created, in UTC. Derived from id, which is a creation stamp in seconds; approximate when many records were created within one second. |
| `updated_at` | `TIMESTAMP` | When the record was last changed, in UTC. NULL when the vault holds no change stamp for it. |
| `booking_date` | `DATE` | Day the bank booked the entry. NULL for an entry that is not booked yet and for a stored date that is no calendar date. |
| `value_date` | `DATE` | Value date. NULL when the bank gave none, and always for cash accounts. |
| `status` | `VARCHAR` | booked, pending (announced by the bank, not booked yet) or information (a notice that moves no money). |
| `pending` | `BOOLEAN` | True when status is not booked. Pending entries are not part of the account's stored balance. |
| `amount` | `DECIMAL(18,4)` | Signed amount in `currency`: negative when money leaves the account, positive when it arrives. Rounded to four decimals if the stored value has more. |
| `debit` | `BOOLEAN` | Direction: true when money leaves the account (amount is negative or zero), false when it arrives. |
| `stored_amount` | `DECIMAL(28,10)` | The amount as the vault stores it, without the direction; normally not negative. |
| `currency` | `VARCHAR` | Currency of `amount`: the account's currency, which is what Banking4 shows, in a PayPal account too. The stored code for a booking whose account has no currency or no row in `accounts`. |
| `stored_currency` | `VARCHAR` | The currency code stored in the record. Banking4 keeps it only in PayPal accounts, where it can differ from `currency`; elsewhere it replaces it with the account's. |
| `payee_name` | `VARCHAR` | Name of the other party: the recipient of a debit, the sender of a credit. |
| `payee_iban` | `VARCHAR` | Account of the other party: an IBAN, or a national account number in old records. |
| `payee_bic` | `VARCHAR` | Bank of the other party: a BIC, or a national bank code in old records. |
| `payee_country` | `VARCHAR` | Country code of the other party. |
| `payee_street` | `VARCHAR` | Street of the other party's address. |
| `payee_town` | `VARCHAR` | Postal code and town of the other party's address. |
| `ultimate_payee_name` | `VARCHAR` | Name of the party on whose behalf the other party acts (SEPA ultimate creditor or debtor). |
| `purpose` | `VARCHAR` | Remittance text; several lines are joined with a line feed. |
| `category` | `VARCHAR` | Category as the record names it: the full name with levels separated by `:`, such as `Wohnen:Miete`. NULL when uncategorised. Need not exist in `categories`. |
| `category_key` | `VARCHAR` | Matching key of `category`: the name trimmed and upper-cased. Join it to categories.category_key; names that differ only in case are one category. |
| `booking_text` | `VARCHAR` | The bank's name for the kind of transaction, such as `Lastschrift`. |
| `notes` | `VARCHAR` | Note the user attached to the entry. |
| `counted` | `BOOLEAN` | False for the master row of a split or batch that has parts: its parts carry the money. Sum `amount` over rows where counted is true, or money is counted twice. |
| `split_role` | `VARCHAR` | Role in a split or batch: split_master, split_slave, batch_master or batch_slave. NULL for an ordinary entry. |
| `master_id` | `BIGINT` | id of the master row of the split or batch the entry belongs to, in the same account. A master points at itself. NULL for an ordinary entry. |
| `batch_index` | `BIGINT` | Position within its split or batch: 0 for the master and for ordinary entries, 1 and up for the parts. |
| `flag` | `VARCHAR` | Colour mark the user set: none, ok, red, blue, yellow, green, gray or purple. |
| `is_read` | `BOOLEAN` | True when the user has seen the entry. |
| `entry_bits` | `VARCHAR` | Processing marks as flag names: read, exported_to_books, uploaded. |
| `transaction_code` | `VARCHAR` | Kind of transaction as stored: other, remittance, debit, withdrawal_or_deposit, check, standing_order, reversal, charges, cash_management or card_payment. |
| `gvc` | `BIGINT` | The German banks' business transaction code (Geschäftsvorfallcode); 0 when none. |
| `gvc_extension` | `BIGINT` | Extension of the business transaction code. |
| `swift_transaction_code` | `VARCHAR` | SWIFT transaction type of the statement line, such as NTRF. |
| `primanota` | `VARCHAR` | The bank's journal number. |
| `bank_reference` | `VARCHAR` | The bank's reference for the entry. |
| `end_to_end_id` | `VARCHAR` | SEPA end-to-end reference. |
| `payment_info_id` | `VARCHAR` | SEPA payment information id, shared by the transfers of one batch. |
| `mandate_id` | `VARCHAR` | Mandate reference of a direct debit. |
| `mandate_signed_date` | `DATE` | Day the direct debit mandate was signed. |
| `creditor_id` | `VARCHAR` | SEPA creditor identifier of a direct debit. |
| `purpose_code` | `VARCHAR` | SEPA purpose code, such as SALA. |
| `category_purpose_code` | `VARCHAR` | SEPA category purpose code. |
| `local_instrument_code` | `VARCHAR` | SEPA local instrument code, such as CORE or B2B. |
| `sequence_type` | `VARCHAR` | SEPA direct debit sequence type, such as FRST or RCUR. |
| `order_id` | `VARCHAR` | Order number the bank assigned. |
| `transaction_date` | `DATE` | Day the transaction took place, for card payments. NULL when not given, and always for cash accounts. |
| `analysis_date` | `DATE` | Day the user assigned the entry to for evaluations, when it differs from the booking date. NULL when not set. |
| `vat_amount` | `DECIMAL(18,4)` | Value added tax contained in the amount, without sign; 0 when none. |
| `vat_rate_type` | `VARCHAR` | Kind of tax rate as flag names. |
| `instructed_amount` | `DECIMAL(18,4)` | Original amount in `instructed_currency` when the transaction was in another currency; 0 when none. Sign as delivered by the bank. |
| `instructed_currency` | `VARCHAR` | Currency of `instructed_amount`. |
| `charges_amount` | `DECIMAL(18,4)` | Charges contained in the entry, in `charges_currency`; 0 when none. |
| `charges_currency` | `VARCHAR` | Currency of `charges_amount`. |
| `virtual_iban` | `VARCHAR` | Virtual IBAN the payment was addressed to. |
| `debit_return_order_id` | `VARCHAR` | Order number of a returned direct debit. |
| `debit_return_date` | `DATE` | Day a direct debit was returned. |
| `debit_return_time` | `TIME` | Time of day a direct debit was returned. |
| `cost_bearer` | `VARCHAR` | Who pays the charges of a foreign payment: none, ours, shared or beneficiary. |
| `intl_remittance_method` | `VARCHAR` | Remittance method of a foreign payment. |
| `intl_remittance_instruction` | `VARCHAR` | Instruction text of a foreign payment. |
| `regulatory_reporting_code` | `VARCHAR` | Regulatory reporting code of a foreign payment. |
| `payee_bank_name` | `VARCHAR` | Name of the other party's bank. |
| `payee_bank_street` | `VARCHAR` | Street of the other party's bank. |
| `payee_bank_town` | `VARCHAR` | Town of the other party's bank. |
| `account_name` | `VARCHAR` | Name of the account (accounts.name). NULL when the account has no row. |
| `account_iban` | `VARCHAR` | IBAN of the account (accounts.iban). |
| `account_kind` | `VARCHAR` | Kind of the account (accounts.kind), such as bank or credit_card. |
| `account_currency` | `VARCHAR` | Currency of the account (accounts.currency). |
| `account_hidden` | `BOOLEAN` | True when the account is hidden in Banking4 (accounts.hidden). |
| `booking_month` | `DATE` | First day of the month of booking_date, for grouping by month. NULL without a booking date. |
| `category_name` | `VARCHAR` | Last level of `category`: `Strom` for `Wohnen:Nebenkosten:Strom`. |
| `category_parent` | `VARCHAR` | The category's parent path: `Wohnen:Nebenkosten` for `Wohnen:Nebenkosten:Strom`. NULL for a top-level category and for an uncategorised booking. |
| `category_top` | `VARCHAR` | First level of `category`: `Wohnen` for `Wohnen:Nebenkosten:Strom`. Group by it for totals per main category. |
| `category_special` | `VARCHAR` | categories.special of the booking's category: reclassification marks transfers between own accounts, excluded marks bookings left out of evaluations. NULL when the category has no row in `categories`. |

### `account_balances` (view)

One row per account: the balance stored in the vault next to the sum of the account's bookings. The two agree when the vault holds every booking since the account started at zero; a constant `difference` is the balance before the first stored booking.

| Column | Type | Description |
|---|---|---|
| `account_id` | `BIGINT` | The account (accounts.id). |
| `name` | `VARCHAR` | Name of the account. |
| `kind` | `VARCHAR` | Kind of the account. |
| `currency` | `VARCHAR` | Currency of all amounts of the row. |
| `hidden` | `BOOLEAN` | True when the account is hidden in Banking4. |
| `balance` | `DECIMAL(18,4)` | Balance according to the last statement, as stored (accounts.balance). |
| `balance_at` | `TIMESTAMP` | Moment the stored balance refers to; zone unknown. |
| `booking_count` | `BIGINT` | Number of rows in `bookings` for the account, of every status and role. |
| `booked_sum` | `DECIMAL(38,4)` | Sum of the signed amounts of the booked, counted bookings. |
| `pending_sum` | `DECIMAL(38,4)` | Sum of the signed amounts of the counted bookings that are not booked yet. |
| `difference` | `DECIMAL(38,4)` | balance minus booked_sum: 0 when the bookings explain the stored balance. |
| `first_booking_date` | `DATE` | Earliest booking date of the account. NULL without dated bookings. |
| `last_booking_date` | `DATE` | Latest booking date of the account. NULL without dated bookings. |

### `monthly_category_sums` (view)

Sums of the booked, counted bookings per month, category and currency, over all accounts. Transfers between own accounts are included under their category; leave them out by category or use bookings_view with category_special.

| Column | Type | Description |
|---|---|---|
| `month` | `DATE` | First day of the month of the booking date. |
| `category` | `VARCHAR` | Full category name. NULL for the uncategorised bookings. |
| `category_key` | `VARCHAR` | Matching key of the category; joins to categories.category_key. |
| `currency` | `VARCHAR` | Currency of the sums. |
| `booking_count` | `BIGINT` | Number of bookings in the group. |
| `credits` | `DECIMAL(38,4)` | Sum of the money that arrived; positive or 0. |
| `debits` | `DECIMAL(38,4)` | Sum of the money that left; negative or 0. |
| `net` | `DECIMAL(38,4)` | credits plus debits. |
