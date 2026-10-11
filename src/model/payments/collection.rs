//! What Banking4 does to the bookings and payments of one account after it
//! has read them all (04 §6.4, §8.3).
//!
//! Banking4 sorts the list, re-derives which records form a group, takes
//! records out of groups that do not hold together and drops payments in one
//! legacy state. `b4` keeps every record a file holds and keeps the file
//! order, so the functions here *mark* records instead: they fill in
//! `batch_index`, `ungrouped_by_banking4` and `hidden_by_banking4`. The
//! order Banking4 would show is available separately.
//!
//! Standing orders are neither grouped nor changed by their collection.
//! Banking4 sorts them by a date it computes from the schedule and the
//! current date (04 §6.4); that rule is not known, so there is no function
//! for it.

use std::collections::hash_map::Entry;
use std::collections::HashMap;

use crate::wire::{Date, RawDate, TimeOfDay, Timestamp};

use super::booking::Booking;
use super::enums::{BatchRole, BookingStatus, PaymentFlags, PaymentState};
use super::payment::Payment;

/// Number of 100 ns ticks in one day.
const TICKS_PER_DAY: i64 = 864_000_000_000;

/// What a record contributes to the sort and the regrouping of 04 §6.4.
#[derive(Debug, Clone, Copy)]
struct Member {
    /// Index of the record in the caller's slice.
    index: usize,
    /// Object id.
    oid: i32,
    /// Link to the master as decoded, before any regrouping.
    batch_oid: i32,
    /// The sort date as a tick count.
    sort_ticks: i64,
}

impl Member {
    const fn role(&self) -> BatchRole {
        BatchRole::of(self.oid, self.batch_oid)
    }

    /// The sort key of 04 §6.4 step 1: the date; then the group, which is
    /// the master's id for a grouped record and the own id otherwise; then a
    /// master before its slaves; then the id.
    fn key(&self) -> (i64, i32, bool, i32) {
        let role = self.role();
        let group = match role {
            BatchRole::None => self.oid,
            BatchRole::Master | BatchRole::Slave => self.batch_oid,
        };
        (
            self.sort_ticks,
            group,
            !matches!(role, BatchRole::Master),
            self.oid,
        )
    }
}

/// What the regrouping decides for one record.
#[derive(Debug, Clone, Copy, Default)]
struct Mark {
    batch_index: u32,
    ungrouped: bool,
}

/// How many records the regrouping of 04 §6.4 touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Regrouping {
    /// Masters that are followed by at least one of their slaves.
    pub groups: usize,
    /// Slaves reached through their master.
    pub slaves: usize,
    /// Records Banking4 takes out of their group: masters without a slave
    /// and slaves their master does not reach.
    pub ungrouped: usize,
}

/// Midnight of a stored date as Banking4 converts it (02 §4.12), in ticks. A
/// date that converts to "no date" sorts first.
fn date_ticks(date: Option<RawDate>, today: Date) -> i64 {
    match date {
        None => today.at_midnight().ticks(),
        Some(raw) => raw.timestamp().map_or(0, Timestamp::ticks),
    }
}

/// The last tick of the day `ticks` lies in.
fn end_of_day(ticks: i64) -> i64 {
    // Banking4 fails here for 9999-12-31, whose last instant it cannot
    // compute. Here the result is capped at the last timestamp there is.
    ticks
        .div_euclid(TICKS_PER_DAY)
        .saturating_mul(TICKS_PER_DAY)
        .saturating_add(TICKS_PER_DAY - 1)
        .min(Timestamp::MAX.ticks())
}

/// The members in the order Banking4 sorts them (04 §6.4 step 1). The sort
/// is stable, so records that tie in every part of the key (which takes a
/// repeated object id) stay in file order.
fn sorted(mut members: Vec<Member>) -> Vec<Member> {
    members.sort_by_key(Member::key);
    members
}

/// Walk the sorted members and decide position and membership of each
/// (04 §6.4 steps 2 and 3). The result is indexed like `sorted`.
fn regroup(sorted: &[Member]) -> (Vec<Mark>, Regrouping) {
    let mut marks = vec![Mark::default(); sorted.len()];
    let mut counts = Regrouping::default();
    // Position in `sorted` of the master whose slaves are being collected,
    // and how many it has so far.
    let mut current: Option<(usize, u32)> = None;
    for (position, member) in sorted.iter().enumerate() {
        match member.role() {
            BatchRole::Master => {
                // Until a slave follows, the master is on its own.
                if let Some(mark) = marks.get_mut(position) {
                    mark.ungrouped = true;
                }
                current = Some((position, 0));
            }
            BatchRole::Slave => {
                let master = current
                    .and_then(|(at, slaves)| Some((at, slaves, sorted.get(at)?)))
                    .filter(|(_, _, master)| master.oid == member.batch_oid);
                if let Some((at, slaves, _)) = master {
                    if let Some(mark) = marks.get_mut(position) {
                        mark.batch_index = slaves;
                    }
                    if let Some(mark) = marks.get_mut(at) {
                        mark.ungrouped = false;
                    }
                    current = Some((at, slaves.saturating_add(1)));
                    counts.slaves += 1;
                    counts.groups += usize::from(slaves == 0);
                } else {
                    // The group ends at the first record of another one.
                    if let Some(mark) = marks.get_mut(position) {
                        mark.ungrouped = true;
                    }
                    current = None;
                }
            }
            BatchRole::None => current = None,
        }
    }
    counts.ungrouped = marks.iter().filter(|mark| mark.ungrouped).count();
    (marks, counts)
}

// ---------------------------------------------------------------- bookings

/// The sort date of a booking (04 §6.4 step 1): its booking date, or today
/// without one; for an entry that is not booked, the last instant of that
/// day, so that it follows the booked entries of the day.
fn booking_sort_ticks(booking: &Booking, today: Date) -> i64 {
    let ticks = date_ticks(booking.booking_date, today);
    if booking.booking_status == BookingStatus::Booked {
        ticks
    } else {
        end_of_day(ticks)
    }
}

fn booking_members(bookings: &[Booking], today: Date) -> Vec<Member> {
    bookings
        .iter()
        .enumerate()
        .map(|(index, booking)| Member {
            index,
            oid: booking.base.record.oid,
            batch_oid: booking.batch_oid,
            sort_ticks: booking_sort_ticks(booking, today),
        })
        .collect()
}

/// The order in which Banking4 lists the bookings of one account (04 §6.4
/// step 1), as indexes into `bookings`.
///
/// `today` stands in for the booking date of entries that have none, as in
/// Banking4; it only decides where those entries go.
#[must_use]
pub fn banking4_booking_order(bookings: &[Booking], today: Date) -> Vec<usize> {
    sorted(booking_members(bookings, today))
        .iter()
        .map(|member| member.index)
        .collect()
}

/// Re-derive the splits and batches among the bookings of one account the
/// way Banking4 does after loading them (04 §6.4), and mark each booking
/// with the result: `batch_index` and `ungrouped_by_banking4`.
///
/// Nothing is removed, reordered or overwritten: `split` and `batch_oid`
/// keep what the file holds, and [`Booking::effective_split`],
/// [`Booking::effective_batch_oid`] and [`Booking::is_counted`] give what
/// Banking4 shows. The slice is all bookings of the account, including
/// those of a legacy `StmtMasterEntries` file. Calling it again gives the
/// same marks.
///
/// `today` stands in for the booking date of entries that have none, as in
/// Banking4; it can only matter for groups made of such entries.
pub fn regroup_bookings(bookings: &mut [Booking], today: Date) -> Regrouping {
    let sorted = sorted(booking_members(bookings, today));
    let (marks, counts) = regroup(&sorted);
    for (member, mark) in sorted.iter().zip(marks) {
        if let Some(booking) = bookings.get_mut(member.index) {
            booking.batch_index = mark.batch_index;
            booking.ungrouped_by_banking4 = mark.ungrouped;
        }
    }
    counts
}

// ---------------------------------------------------------------- payments

/// How many payments each rule of 04 §8.3 applied to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PaymentRules {
    /// Payments Banking4 does not show (step 1).
    pub hidden: usize,
    /// Payments whose legacy state was replaced (step 2).
    pub legacy_states: usize,
    /// Payments that had legacy batch bits in their flags (step 4).
    pub legacy_flags: usize,
    /// Batch masters in the legacy representation (step 3).
    pub legacy_masters: usize,
    /// Legacy batch masters whose batch number an earlier master already
    /// had. Banking4 stops loading the file at such a record; here the
    /// earlier master keeps the batch and the later one stays on its own.
    pub legacy_master_conflicts: usize,
    /// Slaves that were linked to a legacy batch master (step 5).
    pub legacy_slaves: usize,
    /// The regrouping of the payments Banking4 shows (step 6).
    pub regrouping: Regrouping,
}

/// What a slave takes over from its legacy batch master (04 §8.3 step 5).
#[derive(Debug, Clone, Copy)]
struct LegacyMaster {
    oid: i32,
    scheduled_date: Option<RawDate>,
    reported_scheduled_time: Option<TimeOfDay>,
}

fn payment_members(payments: &[Payment], today: Date) -> Vec<Member> {
    payments
        .iter()
        .enumerate()
        .filter(|(_, payment)| !payment.hidden_by_banking4)
        .map(|(index, payment)| Member {
            index,
            oid: payment.base.record.oid,
            batch_oid: payment.batch_oid,
            // 04 §6.4: the scheduled date, or today without one; no
            // end-of-day rule for payments.
            sort_ticks: date_ticks(payment.base.scheduled_date, today),
        })
        .collect()
}

/// The order in which Banking4 lists the payments of one account (04 §6.4),
/// as indexes into `payments`. Payments marked `hidden_by_banking4` are left
/// out, so call it after [`apply_payment_rules`].
///
/// `today` stands in for the scheduled date of payments that have none.
#[must_use]
pub fn banking4_payment_order(payments: &[Payment], today: Date) -> Vec<usize> {
    sorted(payment_members(payments, today))
        .iter()
        .map(|member| member.index)
        .collect()
}

/// Apply what Banking4's payment list does while and after loading the
/// payments of one account (04 §8.3).
///
/// - A payment whose state is exactly the legacy "advised direct debit" is
///   marked `hidden_by_banking4`; Banking4 drops it. It takes no part in
///   the other steps.
/// - The two other legacy states are replaced by their current equivalents
///   and the legacy batch bits are cleared from the flags, in place.
/// - Batches in the legacy representation are converted: the master's
///   `batch_oid` becomes its own id, and each of its slaves gets that id as
///   `batch_oid` together with the master's `scheduled_date` and reported
///   execution time, replacing the slave's own. (Banking4 also marks such a
///   master as modified; the update stamp is left alone here.)
/// - Batches are re-derived as for bookings (04 §6.4) and each payment is
///   marked with `batch_index` and `ungrouped_by_banking4`.
///
/// Nothing is removed or reordered. Calling it again changes nothing more.
///
/// `today` stands in for the scheduled date of payments that have none, as
/// in Banking4; it can only matter for batches made of such payments.
pub fn apply_payment_rules(payments: &mut [Payment], today: Date) -> PaymentRules {
    let mut rules = PaymentRules::default();
    let mut legacy_masters: HashMap<i32, LegacyMaster> = HashMap::new();

    for payment in payments.iter_mut() {
        payment.batch_index = 0;
        payment.ungrouped_by_banking4 = false;
        // Step 1: the state is compared as a whole, not bit by bit.
        payment.hidden_by_banking4 = payment.state == PaymentState::LEGACY_ADVISED_DIRECT_DEBIT;
        if payment.hidden_by_banking4 {
            rules.hidden += 1;
            continue;
        }
        // Step 2, again on the whole state.
        let current_state = if payment.state == PaymentState::LEGACY_ADVISED_PAYMENT {
            Some(PaymentState::LOCAL)
        } else if payment.state == PaymentState::LEGACY_POSTDATED_IN_OUTBOX {
            Some(PaymentState::IN_OUTBOX)
        } else {
            None
        };
        if let Some(state) = current_state {
            payment.state = state;
            rules.legacy_states += 1;
        }
        // Step 3: remember a legacy master under its batch number, then make
        // it a master in the current sense.
        if payment.flags.contains(PaymentFlags::LEGACY_BATCH_MASTER) {
            match legacy_masters.entry(payment.batch_oid) {
                Entry::Vacant(entry) => {
                    entry.insert(LegacyMaster {
                        oid: payment.base.record.oid,
                        scheduled_date: payment.base.scheduled_date,
                        reported_scheduled_time: payment.base.reported_scheduled_time(),
                    });
                    rules.legacy_masters += 1;
                }
                Entry::Occupied(_) => rules.legacy_master_conflicts += 1,
            }
            payment.batch_oid = payment.base.record.oid;
        }
        // Step 4.
        let legacy_bits = payment.flags.raw() & PaymentFlags::LEGACY_BATCH_BITS;
        if legacy_bits != 0 {
            payment.flags = PaymentFlags::from_raw(payment.flags.raw() & !legacy_bits);
            rules.legacy_flags += 1;
        }
    }

    // Step 5: only when the file held a legacy master at all.
    if !legacy_masters.is_empty() {
        for payment in payments.iter_mut().filter(|p| !p.hidden_by_banking4) {
            if payment.batch_role() != BatchRole::Slave {
                continue;
            }
            if let Some(master) = legacy_masters.get(&payment.batch_oid) {
                payment.batch_oid = master.oid;
                payment.base.scheduled_date = master.scheduled_date;
                payment.base.scheduled_time = master.reported_scheduled_time;
                rules.legacy_slaves += 1;
            }
        }
    }

    // Step 6.
    let sorted = sorted(payment_members(payments, today));
    let (marks, counts) = regroup(&sorted);
    for (member, mark) in sorted.iter().zip(marks) {
        if let Some(payment) = payments.get_mut(member.index) {
            payment.batch_index = mark.batch_index;
            payment.ungrouped_by_banking4 = mark.ungrouped;
        }
    }
    rules.regrouping = counts;
    rules
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(index: usize, oid: i32, batch_oid: i32, day: i64) -> Member {
        Member {
            index,
            oid,
            batch_oid,
            sort_ticks: day * TICKS_PER_DAY,
        }
    }

    /// Sort, regroup, and give `(batch_index, ungrouped)` per input index.
    fn marks_of(members: Vec<Member>) -> (Vec<(u32, bool)>, Regrouping) {
        let sorted = sorted(members);
        let (marks, counts) = regroup(&sorted);
        let mut by_index = vec![(0, false); sorted.len()];
        for (member, mark) in sorted.iter().zip(marks) {
            by_index[member.index] = (mark.batch_index, mark.ungrouped);
        }
        (by_index, counts)
    }

    #[test]
    fn a_master_is_sorted_before_its_slaves_and_numbers_them() {
        // In file order: slave 12, an unrelated record, slave 11, and the
        // master, whose id is higher than those of its slaves: only the
        // master-first rule puts it in front of them.
        let members = vec![
            member(0, 12, 13, 5),
            member(1, 5, 0, 5),
            member(2, 11, 13, 5),
            member(3, 13, 13, 5),
        ];
        let order: Vec<usize> = sorted(members.clone()).iter().map(|m| m.index).collect();
        assert_eq!(order, [1, 3, 2, 0]);
        let (marks, counts) = marks_of(members);
        assert_eq!(marks, [(1, false), (0, false), (0, false), (0, false)]);
        assert_eq!(
            counts,
            Regrouping {
                groups: 1,
                slaves: 2,
                ungrouped: 0
            }
        );
    }

    #[test]
    fn a_master_without_slaves_and_an_unreached_slave_are_ungrouped() {
        let members = vec![
            // A master alone.
            member(0, 10, 10, 1),
            // A slave whose master is on another day.
            member(1, 21, 20, 2),
            member(2, 20, 20, 3),
            member(3, 22, 20, 3),
            // A slave without any master.
            member(4, 31, 30, 4),
        ];
        let (marks, counts) = marks_of(members);
        assert_eq!(
            marks,
            [(0, true), (0, true), (0, false), (0, false), (0, true)]
        );
        assert_eq!(
            counts,
            Regrouping {
                groups: 1,
                slaves: 1,
                ungrouped: 3
            }
        );
    }

    #[test]
    fn a_group_ends_at_the_first_record_of_another_group() {
        // Master 10 with slave 11, then master 20 between them and slave 12:
        // sorted by group id, 12 still follows 11 directly.
        let members = vec![
            member(0, 10, 10, 1),
            member(1, 11, 10, 1),
            member(2, 12, 10, 1),
            member(3, 20, 20, 1),
            member(4, 21, 20, 1),
        ];
        let (marks, _) = marks_of(members);
        assert_eq!(
            marks,
            [(0, false), (0, false), (1, false), (0, false), (0, false)]
        );
    }

    #[test]
    fn links_of_zero_and_minus_one_are_no_group() {
        let (marks, counts) = marks_of(vec![member(0, 10, 0, 1), member(1, 11, -1, 1)]);
        assert_eq!(marks, [(0, false), (0, false)]);
        assert_eq!(counts, Regrouping::default());
    }

    #[test]
    fn the_end_of_a_day_never_overflows() {
        assert_eq!(end_of_day(0), TICKS_PER_DAY - 1);
        assert_eq!(end_of_day(TICKS_PER_DAY), 2 * TICKS_PER_DAY - 1);
        assert_eq!(end_of_day(Timestamp::MAX.ticks()), Timestamp::MAX.ticks());
        assert_eq!(end_of_day(i64::MAX), Timestamp::MAX.ticks());
        assert!(end_of_day(i64::MIN) < 0);
    }
}
