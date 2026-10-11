//! Coded values of the payment family (04 §11).
//!
//! Every type keeps a number it has no name for, so a record written by a
//! newer program version still decodes (README, "Coping with other versions").

use crate::model::base::{code_enum, flag_set};

code_enum! {
    /// The class nibble stored in the first byte of the payment base
    /// (04 §11.1). Banking4 does not keep it: it only serves to derive a
    /// missing scheme type (04 §5.4) and to interpret legacy text keys
    /// (04 §5.8).
    pub enum SchemeClass: u8 {
        /// No particular scheme.
        Any = 0 => "any",
        /// A SEPA payment.
        Sepa = 1 => "sepa",
        /// A foreign payment in the legacy German file format.
        ForeignLegacy = 2 => "foreign_legacy",
        /// A domestic payment in the legacy German file format.
        DomesticLegacy = 3 => "domestic_legacy",
    }
}

code_enum! {
    /// The payment scheme of a payment or standing order (04 §11.2). A
    /// booking always has `None` after reading (04 §5.4).
    pub enum SchemeType: i32 {
        /// No scheme: the direction is the stored bit.
        None = 0 => "none",
        /// An expense the user entered by hand.
        UserExpense = 1 => "user_expense",
        /// An income the user entered by hand.
        UserIncome = 2 => "user_income",
        /// A credit transfer in the legacy domestic format.
        DomesticLegacyCreditTransfer = 3 => "domestic_legacy_credit_transfer",
        /// A credit transfer in the legacy foreign format.
        ForeignLegacyCreditTransfer = 4 => "foreign_legacy_credit_transfer",
        /// A SEPA credit transfer.
        SepaCreditTransfer = 5 => "sepa_credit_transfer",
        /// A SEPA core direct debit.
        SepaDirectDebitCore = 6 => "sepa_direct_debit_core",
        /// A SEPA business-to-business direct debit.
        SepaDirectDebitB2b = 7 => "sepa_direct_debit_b2b",
        /// A SEPA instant credit transfer.
        SepaInstantCreditTransfer = 8 => "sepa_instant_credit_transfer",
        /// An urgent credit transfer.
        UrgentCreditTransfer = 9 => "urgent_credit_transfer",
        /// A foreign credit transfer.
        ForeignCreditTransfer = 10 => "foreign_credit_transfer",
        /// A Swiss credit transfer.
        SwissCreditTransfer = 11 => "swiss_credit_transfer",
    }
}

impl SchemeType {
    /// The direction the scheme implies (04 §5.5, §11.2): `Some(true)` for a
    /// debit, `Some(false)` for a credit, `None` when there is no scheme and
    /// the stored direction bit decides. A scheme without a name is a debit,
    /// as in Banking4.
    #[must_use]
    pub const fn debit(self) -> Option<bool> {
        match self {
            Self::None => None,
            Self::UserIncome | Self::SepaDirectDebitCore | Self::SepaDirectDebitB2b => Some(false),
            _ => Some(true),
        }
    }

    /// The class Banking4 writes for the scheme (04 §11.2, §5.9). It can
    /// differ from the class nibble a record was read with.
    #[must_use]
    pub const fn class(self) -> SchemeClass {
        match self {
            Self::DomesticLegacyCreditTransfer => SchemeClass::DomesticLegacy,
            Self::ForeignLegacyCreditTransfer => SchemeClass::ForeignLegacy,
            Self::SepaCreditTransfer
            | Self::SepaDirectDebitCore
            | Self::SepaDirectDebitB2b
            | Self::SepaInstantCreditTransfer
            | Self::UrgentCreditTransfer
            | Self::ForeignCreditTransfer
            | Self::SwissCreditTransfer => SchemeClass::Sepa,
            Self::None | Self::UserExpense | Self::UserIncome | Self::Unknown(_) => {
                SchemeClass::Any
            }
        }
    }
}

code_enum! {
    /// Who pays the charges of a foreign payment (04 §11.3).
    pub enum CostBearer: u8 {
        /// Not stated.
        None = 0 => "none",
        /// The payer (`OUR`).
        Ours = 1 => "ours",
        /// Shared between payer and payee (`SHA`).
        Shared = 2 => "shared",
        /// The payee (`BEN`).
        Beneficiary = 3 => "beneficiary",
    }
}

code_enum! {
    /// Whether the bank has booked an entry (04 §11.4).
    pub enum BookingStatus: u8 {
        /// Booked.
        Booked = 0 => "booked",
        /// Announced by the bank, not booked yet.
        Pending = 1 => "pending",
        /// For information only.
        Information = 2 => "information",
    }
}

flag_set! {
    /// Markers of a booking (04 §11.5).
    pub struct EntryBits: u8 {
        /// The user has seen the booking.
        READ = 0x01 => "read",
        /// The booking was exported to an accounting program.
        EXPORTED_TO_BOOKS = 0x02 => "exported_to_books",
        /// The booking was uploaded to a document service.
        UPLOADED = 0x04 => "uploaded",
    }
}

code_enum! {
    /// The colour mark of a booking or payment (04 §11.6).
    pub enum ColorFlag: i32 {
        /// Banking4's in-memory marker for "not determined"; not a colour.
        Undefined = -1 => "undefined",
        /// No mark.
        None = 0 => "none",
        /// The check mark.
        Ok = 1 => "ok",
        /// Red.
        Red = 2 => "red",
        /// Blue.
        Blue = 3 => "blue",
        /// Yellow.
        Yellow = 4 => "yellow",
        /// Green.
        Green = 5 => "green",
        /// Gray.
        Gray = 6 => "gray",
        /// Purple.
        Purple = 7 => "purple",
    }
}

code_enum! {
    /// The kind of transaction as stored with a booking (04 §11.7). Banking4
    /// may display another one, computed from other fields (04 §6.6).
    pub enum TransactionCode: i32 {
        /// Anything else.
        Other = 0 => "other",
        /// A credit transfer.
        Remittance = 1 => "remittance",
        /// A direct debit.
        Debit = 2 => "debit",
        /// A cash withdrawal or deposit.
        WithdrawalOrDeposit = 3 => "withdrawal_or_deposit",
        /// A cheque.
        Check = 4 => "check",
        /// An execution of a standing order.
        StandingOrder = 5 => "standing_order",
        /// A reversal.
        Reversal = 6 => "reversal",
        /// Charges and interest.
        Charges = 7 => "charges",
        /// A cash management transfer.
        CashManagement = 8 => "cash_management",
        /// A card payment.
        CardPayment = 9 => "card_payment",
    }
}

flag_set! {
    /// The role of a booking in a split or batch as stored (04 §11.8).
    pub struct SplitRole: u8 {
        /// The booking the user divided into parts.
        SPLIT_MASTER = 0x01 => "split_master",
        /// One part of a divided booking.
        SPLIT_SLAVE = 0x02 => "split_slave",
        /// A collective booking matched with a batch of payments.
        BATCH_MASTER = 0x10 => "batch_master",
        /// One payment of such a batch.
        BATCH_SLAVE = 0x20 => "batch_slave",
    }
}

impl SplitRole {
    /// Whether the booking is stored as a master of either kind, the mask of
    /// 04 §6.2 step 6.
    #[must_use]
    pub const fn is_any_master(self) -> bool {
        self.raw() & (Self::SPLIT_MASTER.raw() | Self::BATCH_MASTER.raw()) != 0
    }
}

flag_set! {
    /// The VAT treatment of a booking (04 §11.9).
    pub struct VatRateType: u8 {
        /// The standard rate applies.
        STANDARD = 0x01 => "standard",
        /// Reverse charge.
        REVERSE_CHARGE = 0x02 => "reverse_charge",
        /// Not taxable.
        NON_TAXABLE = 0x04 => "non_taxable",
        /// Subject to VAT accounting.
        VAT_ACCOUNTING = 0x08 => "vat_accounting",
    }
}

flag_set! {
    /// The processing state of a payment (04 §11.10).
    pub struct PaymentState: u16 {
        /// Entered, not submitted.
        LOCAL = 0x0001 => "local",
        /// Waiting in the outbox.
        IN_OUTBOX = 0x0002 => "in_outbox",
        /// Legacy: a post-dated payment in the outbox. Banking4 turns it into
        /// `in_outbox` while loading (04 §8.3).
        LEGACY_POSTDATED_IN_OUTBOX = 0x0004 => "legacy_postdated_in_outbox",
        /// The cancellation of a post-dated payment waits in the outbox.
        CANCEL_POSTDATED_IN_OUTBOX = 0x0008 => "cancel_postdated_in_outbox",
        /// Legacy: an advised payment. Banking4 turns it into `local` while
        /// loading (04 §8.3).
        LEGACY_ADVISED_PAYMENT = 0x0010 => "legacy_advised_payment",
        /// Submitted with a future execution date and held at the bank.
        POSTDATED_AT_BANK = 0x0020 => "postdated_at_bank",
        /// Sent to the bank.
        SENT = 0x0040 => "sent",
        /// Legacy: an advised direct debit. Banking4 drops such a payment
        /// while loading (04 §8.3).
        LEGACY_ADVISED_DIRECT_DEBIT = 0x0080 => "legacy_advised_direct_debit",
        /// A post-dated payment the bank no longer lists.
        ORPHANED_POSTDATED = 0x0100 => "orphaned_postdated",
        /// Matched with a booking.
        RECONCILED = 0x0200 => "reconciled",
    }
}

flag_set! {
    /// Flags of a payment (04 §11.11).
    pub struct PaymentFlags: u16 {
        /// The payment carries a warning.
        WARNING = 0x0001 => "warning",
        /// Legacy: marked for deletion.
        LEGACY_DELETE = 0x0002 => "legacy_delete",
        /// Legacy: a slave of a batch in the old representation. Cleared by
        /// Banking4 while loading (04 §8.3).
        LEGACY_BATCH_SLAVE = 0x0004 => "legacy_batch_slave",
        /// Legacy: the master of a batch in the old representation. Cleared
        /// by Banking4 while loading (04 §8.3).
        LEGACY_BATCH_MASTER = 0x0008 => "legacy_batch_master",
        /// Legacy: the batch was shown expanded. Cleared by Banking4 while
        /// loading (04 §8.3).
        LEGACY_BATCH_EXPANDED = 0x0010 => "legacy_batch_expanded",
        /// The bank requires further signatures.
        NEEDS_MORE_AUTHORISATION = 0x0100 => "needs_more_authorisation",
        /// Reported to an accounting program.
        CONFIRMED_TO_ACCOUNTING = 0x1000 => "confirmed_to_accounting",
    }
}

impl PaymentFlags {
    /// The three legacy batch bits Banking4 clears while loading (04 §8.3
    /// step 4).
    pub const LEGACY_BATCH_BITS: u16 = Self::LEGACY_BATCH_SLAVE.raw()
        | Self::LEGACY_BATCH_MASTER.raw()
        | Self::LEGACY_BATCH_EXPANDED.raw();
}

code_enum! {
    /// Whether a batch is to be booked as one entry (04 §11.12).
    pub enum BatchBooking: u8 {
        /// The bank's default.
        Default = 0 => "default",
        /// One collective entry.
        Yes = 1 => "yes",
        /// One entry per payment.
        No = 2 => "no",
        /// The payments of the batch disagree.
        Mixed = 3 => "mixed",
    }
}

code_enum! {
    /// The status from a payment status report, as the number Banking4 keeps
    /// for the stored four-letter code (04 §11.13).
    pub enum SepaStatus: i32 {
        /// A stored code Banking4 does not know.
        Unrecognised = -1 => "unknown",
        /// No status stored.
        None = 0 => "none",
        /// `RCVD`.
        Received = 1 => "received",
        /// `PDNG`.
        Pending = 2 => "pending",
        /// `PART`.
        PartiallyAccepted = 3 => "partially_accepted",
        /// `ACTC`.
        AcceptedTechnicalValidation = 4 => "accepted_technical_validation",
        /// `ACCP`.
        AcceptedCustomerProfile = 5 => "accepted_customer_profile",
        /// `ACWC`.
        AcceptedWithChange = 6 => "accepted_with_change",
        /// `ACSP`.
        AcceptedSettlementInProcess = 7 => "accepted_settlement_in_process",
        /// `ACCC`.
        AcceptedSettlementCompletedCreditor = 8 => "accepted_settlement_completed_creditor",
        /// `ACSC`.
        AcceptedSettlementCompletedDebtor = 9 => "accepted_settlement_completed_debtor",
        /// `RJCT`.
        Rejected = 10 => "rejected",
    }
}

impl SepaStatus {
    /// The status for a stored code text (04 §8.1 position 16): no text is
    /// `None`, a text that is not one of the ten codes is `Unrecognised`. The
    /// comparison is exact and case-sensitive.
    #[must_use]
    pub fn from_code(code: Option<&str>) -> Self {
        match code {
            None => Self::None,
            Some("RCVD") => Self::Received,
            Some("PDNG") => Self::Pending,
            Some("PART") => Self::PartiallyAccepted,
            Some("ACTC") => Self::AcceptedTechnicalValidation,
            Some("ACCP") => Self::AcceptedCustomerProfile,
            Some("ACWC") => Self::AcceptedWithChange,
            Some("ACSP") => Self::AcceptedSettlementInProcess,
            Some("ACCC") => Self::AcceptedSettlementCompletedCreditor,
            Some("ACSC") => Self::AcceptedSettlementCompletedDebtor,
            Some("RJCT") => Self::Rejected,
            Some(_) => Self::Unrecognised,
        }
    }
}

code_enum! {
    /// The user's choice for the verification of payee (04 §11.14).
    pub enum PayeeVerificationChoice: u8 {
        /// The default of the bank access.
        Default = 0 => "default",
        /// Verify the payee.
        OptIn = 1 => "opt_in",
        /// Do not verify the payee.
        OptOut = 2 => "opt_out",
    }
}

flag_set! {
    /// The state of a standing order (04 §11.15).
    pub struct StandingOrderState: i32 {
        /// Kept in Banking4 only, not at the bank.
        LOCAL = 0x0_0001 => "local",
        /// The order to set it up waits in the outbox.
        SETUP_IN_OUTBOX = 0x0_0002 => "setup_in_outbox",
        /// A change waits in the outbox.
        CHANGE_IN_OUTBOX = 0x0_0004 => "change_in_outbox",
        /// The cancellation waits in the outbox.
        CANCEL_IN_OUTBOX = 0x0_0008 => "cancel_in_outbox",
        /// Held at the bank.
        AT_BANK = 0x0_0010 => "at_bank",
        /// Derived by Banking4 from recurring bookings.
        AUTO_DETECTED = 0x0_0020 => "auto_detected",
        /// The standing order carries a warning.
        WARNING = 0x1_0000 => "warning",
        /// Deleted.
        DELETED = 0x2_0000 => "deleted",
    }
}

code_enum! {
    /// The unit of a standing order's period (04 §11.16). The numbers are the
    /// character codes of `M` and `W`.
    pub enum PeriodUnit: i32 {
        /// Not set.
        None = 0 => "none",
        /// Months.
        Month = 77 => "month",
        /// Weeks.
        Week = 87 => "week",
    }
}

/// Whether a record belongs to a group of records, by its stored link alone
/// (04 §5.10): a split or batch of bookings, a batch of payments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatchRole {
    /// Not in a group.
    None,
    /// The record the others belong to: its link names itself.
    Master,
    /// A member of a group: its link names the master.
    Slave,
}

impl BatchRole {
    /// The role that follows from a record's object id and its link to a
    /// master. A link of 0 or -1 means "not in a group".
    #[must_use]
    pub const fn of(oid: i32, batch_oid: i32) -> Self {
        if batch_oid == 0 || batch_oid == -1 {
            Self::None
        } else if batch_oid == oid {
            Self::Master
        } else {
            Self::Slave
        }
    }
}
