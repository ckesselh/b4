//! The coded values of accounts and account groups (03 "Enumerations").
//!
//! Only single values and single bits are listed. The masks and aliases the
//! document mentions are combinations of them and appear here as methods
//! where a reader rule needs one.

use crate::model::base::{code_enum, flag_set};

code_enum! {
    /// What an account is (03 "Account kind").
    ///
    /// On the wire this is a bit set of which a stored account carries exactly
    /// one bit, so it is modelled as an enumeration: a value with several bits
    /// is `Unknown` and keeps its number. `None` marks the placeholder record
    /// of a migrated `Accts` file, which is never a real account.
    pub enum AccountKind: u8 {
        /// No kind: the placeholder record, dropped by every loader.
        None = 0x00 => "none",
        /// An account at a bank.
        Bank = 0x01 => "bank",
        /// Cash kept by hand, without online access.
        Cash = 0x02 => "cash",
        /// A credit card account.
        CreditCard = 0x04 => "credit_card",
        /// A securities portfolio.
        Portfolio = 0x08 => "portfolio",
        /// A list of assets.
        Asset = 0x10 => "asset",
        /// A PayPal account.
        Paypal = 0x20 => "paypal",
        /// A crypto currency wallet.
        Crypto = 0x40 => "crypto",
    }
}

impl AccountKind {
    /// The kinds that have bookings: bank, cash, credit card, PayPal, crypto
    /// (03, alias "has statements").
    const STATEMENT_KINDS: u8 = 0x67;
    /// The kinds that have payments and standing orders: bank, credit card,
    /// PayPal (03, alias "has payments").
    const PAYMENT_KINDS: u8 = 0x25;

    /// Whether accounts of this kind have a booking file (`Stmt`). The test
    /// is on the bits, so it also answers for a value with several of them.
    #[must_use]
    pub const fn has_statements(self) -> bool {
        self.raw() & Self::STATEMENT_KINDS != 0
    }

    /// Whether accounts of this kind have payments and standing orders
    /// (`Paymts`, `Stos`).
    #[must_use]
    pub const fn has_payments(self) -> bool {
        self.raw() & Self::PAYMENT_KINDS != 0
    }
}

flag_set! {
    /// Properties of an account (03 "Account flags").
    pub struct AccountFlags: i32 {
        /// The account is hidden in the application.
        HIDDEN = 0x0000_0001 => "hidden",
        /// The account does not count towards totals.
        EXCLUDE_FROM_TOTALS = 0x0000_0002 => "exclude_from_totals",
        /// A setting the application no longer supports.
        LEGACY_STATEMENT_AFTER_ORDER = 0x0000_0004 => "legacy_statement_after_order",
        /// Show the holder's name with the account.
        SHOW_HOLDER_NAME = 0x0000_0008 => "show_holder_name",
        /// Remind about the bank access used for downloads.
        NAGGING_CONTACT = 0x0000_0010 => "nagging_contact",
        /// Remind about the bank access used for orders.
        NAGGING_ORDER_CONTACT = 0x0000_0020 => "nagging_order_contact",
        /// The account is not part of "fetch everything".
        EXCLUDE_FROM_GENERAL_SYNC = 0x0000_0040 => "exclude_from_general_sync",
        /// The account does not count towards category totals.
        EXCLUDE_FROM_CATEGORY_TOTALS = 0x0000_0080 => "exclude_from_category_totals",
        /// Statement schedule: weekly. See [`AccountFlags::statement_schedule`].
        STATEMENT_SCHEDULE_WEEKLY = 0x0000_0100 => "statement_schedule_weekly",
        /// Statement schedule: monthly. See [`AccountFlags::statement_schedule`].
        STATEMENT_SCHEDULE_MONTHLY = 0x0000_0200 => "statement_schedule_monthly",
        /// Show the summary items of the account.
        SHOW_SUMMARY_ITEMS = 0x0000_0800 => "show_summary_items",
        /// Statements are not acknowledged to the bank.
        DO_NOT_ACKNOWLEDGE_STATEMENTS = 0x0000_1000 => "do_not_acknowledge_statements",
        /// Payee verification is requested for batch transfers.
        VOP_OPT_IN_BATCH_TRANSFERS = 0x0000_2000 => "vop_opt_in_batch_transfers",
        /// Payee verification is declined for single transfers.
        VOP_OPT_OUT_SINGLE_TRANSFERS = 0x0000_4000 => "vop_opt_out_single_transfers",
        /// A setting the application no longer supports.
        LEGACY_INCLUDE_IN_REPLENISH = 0x0100_0000 => "legacy_include_in_replenish",
    }
}

impl AccountFlags {
    /// The three bits that hold the statement schedule.
    const STATEMENT_SCHEDULE_MASK: i32 = 0x0000_0700;

    /// How often statements are fetched: the small field in bits `0x700`.
    #[must_use]
    pub const fn statement_schedule(self) -> StatementSchedule {
        StatementSchedule::from_raw(self.raw() & Self::STATEMENT_SCHEDULE_MASK)
    }
}

code_enum! {
    /// The statement schedule held in three bits of [`AccountFlags`]
    /// (03 "Account flags"). The number is the masked flag value.
    pub enum StatementSchedule: i32 {
        /// With every send/receive run.
        Always = 0x000 => "always",
        /// Once a week.
        Weekly = 0x100 => "weekly",
        /// Once a month. A newly created account starts with this.
        Monthly = 0x200 => "monthly",
    }
}

flag_set! {
    /// Properties of an account group (03 "Group flags").
    pub struct GroupFlags: i32 {
        /// Show the icon of the group's first account for the group.
        SHOW_FIRST_ACCOUNT_ICON = 0x1 => "show_first_account_icon",
        /// The group has no caption of its own.
        ANONYMOUS = 0x2 => "anonymous",
        /// The group's accounts do not count towards totals.
        EXCLUDE_FROM_TOTALS = 0x4 => "exclude_from_totals",
    }
}

flag_set! {
    /// Where an account's limit and available funds come from and which of
    /// them are shown (03 "Balance flags"). Stored in 16 bits.
    pub struct BalanceFlags: u16 {
        /// The overdraft limit was reported by the bank.
        HAS_BANK_OVERDRAFT_LIMIT = 0x0001 => "has_bank_overdraft_limit",
        /// The available funds were reported by the bank.
        HAS_BANK_AVAILABLE_FUNDS = 0x0002 => "has_bank_available_funds",
        /// Show the overdraft limit.
        SHOW_OVERDRAFT_LIMIT = 0x0100 => "show_overdraft_limit",
        /// Show the available funds.
        SHOW_AVAILABLE_FUNDS = 0x0200 => "show_available_funds",
        /// Show the credit interest rate.
        SHOW_CREDIT_INTEREST_RATE = 0x0400 => "show_credit_interest_rate",
    }
}

flag_set! {
    /// Notification preferences of an account (03 "Notification flags").
    /// Stored in 16 bits.
    pub struct NotifyFlags: u16 {
        /// No notifications about single debits and credits.
        DISABLE_DEBIT_CREDIT_NOTIFICATIONS = 0x1 => "disable_debit_credit_notifications",
        /// Notify when the balance leaves the configured range.
        ENABLE_BALANCE_NOTIFICATIONS = 0x2 => "enable_balance_notifications",
        /// Notify about the spending limit. Used by one branded edition only.
        ENABLE_SPENDING_LIMIT_NOTIFICATIONS = 0x4 => "enable_spending_limit_notifications",
    }
}

flag_set! {
    /// Which kinds of data a send/receive run handles for an account
    /// (03 "Sync flags"). Bit `0x1` is not defined.
    pub struct SyncFlags: i32 {
        /// Balances.
        BALANCES = 0x0000_0002 => "balances",
        /// Statements, that is bookings.
        STATEMENTS = 0x0000_0004 => "statements",
        /// Standing orders held at the bank.
        STANDING_ORDERS = 0x0000_0008 => "standing_orders",
        /// Transfers scheduled for a later date.
        POSTDATED_TRANSFERS = 0x0000_0010 => "postdated_transfers",
        /// Securities positions.
        PORTFOLIO = 0x0000_0020 => "portfolio",
        /// Assets.
        ASSETS = 0x0000_0040 => "assets",
        /// Documents such as statements and bank mail.
        FILES = 0x0000_0080 => "files",
        /// Master data of the account.
        ACCOUNT_INFO = 0x0000_0100 => "account_info",
        /// The signature folder.
        PAY_CHECKS = 0x0000_0200 => "pay_checks",
        /// Payees known to the bank.
        PAYEES = 0x0000_0400 => "payees",
        /// Portfolio statements.
        PORTFOLIO_STATEMENT = 0x0000_0800 => "portfolio_statement",
        /// Advice of direct debits.
        DIRECT_DEBIT_ADVICE = 0x0000_1000 => "direct_debit_advice",
        /// Status reports of payments.
        PAYMENT_STATUS_REPORT = 0x0000_2000 => "payment_status_report",
        /// The protocol of the bank access.
        PROTOCOL = 0x1000_0000 => "protocol",
    }
}

impl SyncFlags {
    /// What an old bank account record gains on reading: account info and
    /// payees (03 "Processing after the read", step 3).
    pub(super) const ADDED_TO_OLD_BANK_ACCOUNTS: i32 = 0x0000_0500;
}

flag_set! {
    /// Settings of the bookkeeping-service link of an account
    /// (03 "Bookkeeping-service flags").
    pub struct BookkeepingFlags: i32 {
        /// The account is not synchronised with the bookkeeping service.
        DISABLED = 0x1 => "disabled",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tables of 03 "Enumerations", value by value. The numbers are
    /// written out again here on purpose: a slip in the declarations above
    /// would otherwise go unnoticed.
    #[test]
    fn values_match_the_document() {
        let kinds: [(u8, &str); 8] = [
            (0x00, "none"),
            (0x01, "bank"),
            (0x02, "cash"),
            (0x04, "credit_card"),
            (0x08, "portfolio"),
            (0x10, "asset"),
            (0x20, "paypal"),
            (0x40, "crypto"),
        ];
        for (raw, name) in kinds {
            assert_eq!(AccountKind::from_raw(raw).name(), name);
            assert_eq!(AccountKind::from_raw(raw).raw(), raw);
        }

        let account_flags: [(i32, &str); 15] = [
            (0x0000_0001, "hidden"),
            (0x0000_0002, "exclude_from_totals"),
            (0x0000_0004, "legacy_statement_after_order"),
            (0x0000_0008, "show_holder_name"),
            (0x0000_0010, "nagging_contact"),
            (0x0000_0020, "nagging_order_contact"),
            (0x0000_0040, "exclude_from_general_sync"),
            (0x0000_0080, "exclude_from_category_totals"),
            (0x0000_0100, "statement_schedule_weekly"),
            (0x0000_0200, "statement_schedule_monthly"),
            (0x0000_0800, "show_summary_items"),
            (0x0000_1000, "do_not_acknowledge_statements"),
            (0x0000_2000, "vop_opt_in_batch_transfers"),
            (0x0000_4000, "vop_opt_out_single_transfers"),
            (0x0100_0000, "legacy_include_in_replenish"),
        ];
        for (raw, name) in account_flags {
            assert_eq!(AccountFlags::from_raw(raw).names(), [name]);
        }
        let all = account_flags.iter().fold(0, |bits, (raw, _)| bits | raw);
        assert_eq!(AccountFlags::from_raw(all).unknown_bits(), 0);
        assert_eq!(AccountFlags::from_raw(!all).names().len(), 1);

        let group_flags: [(i32, &str); 3] = [
            (0x1, "show_first_account_icon"),
            (0x2, "anonymous"),
            (0x4, "exclude_from_totals"),
        ];
        for (raw, name) in group_flags {
            assert_eq!(GroupFlags::from_raw(raw).names(), [name]);
        }

        let balance_flags: [(u16, &str); 5] = [
            (0x0001, "has_bank_overdraft_limit"),
            (0x0002, "has_bank_available_funds"),
            (0x0100, "show_overdraft_limit"),
            (0x0200, "show_available_funds"),
            (0x0400, "show_credit_interest_rate"),
        ];
        for (raw, name) in balance_flags {
            assert_eq!(BalanceFlags::from_raw(raw).names(), [name]);
        }

        let notify_flags: [(u16, &str); 3] = [
            (0x1, "disable_debit_credit_notifications"),
            (0x2, "enable_balance_notifications"),
            (0x4, "enable_spending_limit_notifications"),
        ];
        for (raw, name) in notify_flags {
            assert_eq!(NotifyFlags::from_raw(raw).names(), [name]);
        }

        let sync_flags: [(i32, &str); 14] = [
            (0x0000_0002, "balances"),
            (0x0000_0004, "statements"),
            (0x0000_0008, "standing_orders"),
            (0x0000_0010, "postdated_transfers"),
            (0x0000_0020, "portfolio"),
            (0x0000_0040, "assets"),
            (0x0000_0080, "files"),
            (0x0000_0100, "account_info"),
            (0x0000_0200, "pay_checks"),
            (0x0000_0400, "payees"),
            (0x0000_0800, "portfolio_statement"),
            (0x0000_1000, "direct_debit_advice"),
            (0x0000_2000, "payment_status_report"),
            (0x1000_0000, "protocol"),
        ];
        for (raw, name) in sync_flags {
            assert_eq!(SyncFlags::from_raw(raw).names(), [name]);
        }
        // Bit 0 has no name (03 "Sync flags") and is kept as a number.
        assert_eq!(SyncFlags::from_raw(0x1).names(), ["0x1"]);
        assert_eq!(
            SyncFlags::from_raw(SyncFlags::ADDED_TO_OLD_BANK_ACCOUNTS).names(),
            ["account_info", "payees"]
        );

        assert_eq!(BookkeepingFlags::from_raw(0x1).names(), ["disabled"]);
    }

    #[test]
    fn a_kind_with_several_bits_keeps_its_number() {
        let kind = AccountKind::from_raw(0x03);
        assert!(kind.is_unknown());
        assert_eq!(kind.name(), "3");
        assert_eq!(kind.raw(), 0x03);
        // The masks are tests on bits and work for such a value too.
        assert!(kind.has_statements());
    }

    #[test]
    fn kind_masks_follow_the_aliases() {
        let with_statements = [
            AccountKind::Bank,
            AccountKind::Cash,
            AccountKind::CreditCard,
            AccountKind::Paypal,
            AccountKind::Crypto,
        ];
        let with_payments = [
            AccountKind::Bank,
            AccountKind::CreditCard,
            AccountKind::Paypal,
        ];
        for raw in [0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40] {
            let kind = AccountKind::from_raw(raw);
            assert_eq!(kind.has_statements(), with_statements.contains(&kind));
            assert_eq!(kind.has_payments(), with_payments.contains(&kind));
        }
    }

    #[test]
    fn statement_schedule_is_a_field_of_the_account_flags() {
        assert_eq!(
            AccountFlags::from_raw(0x1200).statement_schedule(),
            StatementSchedule::Monthly
        );
        assert_eq!(
            AccountFlags::from_raw(0x0101).statement_schedule(),
            StatementSchedule::Weekly
        );
        assert_eq!(
            AccountFlags::from_raw(0x0001).statement_schedule(),
            StatementSchedule::Always
        );
        // Bit 0x400 belongs to the field but has no meaning of its own.
        assert_eq!(
            AccountFlags::from_raw(0x0400).statement_schedule(),
            StatementSchedule::Unknown(0x400)
        );
    }
}
