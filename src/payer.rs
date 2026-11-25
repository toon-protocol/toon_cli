//! The payer: what pays for a command's packets, and decides what each one cost.
//!
//! A command gives [`pay`] the total it will pay and a closure that sends its packets. Each
//! packet goes through [`Packets::send`] (a send that reports) or [`Packets::answer`] (a send
//! that answers), which price it by one rule:
//!
//! - fulfilled with the right fulfilment: its amount;
//! - rejected, fulfilled with the wrong fulfilment, or unanswered: what the outbound
//!   watermarks moved by across it, at most its amount;
//! - failed certainly before paying: nothing;
//! - any other failure: what moved, and it stays counted in full against the limit.
//!
//! What moved across a packet is what the watermarks moved by beyond what the command's
//! packets before it cost. The command's cost is the sum over its packets ([`Packets::paid`]),
//! and the payer settles the limit once, giving back what did not move. The command decides what
//! to send and when to stop, and writes its own report text.
//!
//! The only payer today is the wallet, whose spending limit `toon` checks ([`Wallet`]). A
//! [`Purse`] is the seam behind it: the limit, the day's ledger and the watermarks.

use std::cell::Cell;
use std::path::Path;

use serde_json::json;

use crate::operator::{self, Answer};
use crate::outcome::{Error, Report};
use crate::spending;

/// The limit, the ledger and the watermarks a payer pays from.
pub trait Purse {
    /// Count `total` against the limit, which needs `yes` and must fit both limits. The
    /// token it returns is handed to [`Purse::refund`].
    fn charge(&self, total: u128, yes: bool) -> Result<u64, Error>;
    /// Give `amount` back to what [`Purse::charge`] counted it on.
    fn refund(&self, token: u64, amount: u128);
    /// Run `act` with what the outbound watermarks have moved by since, at most a cap given,
    /// with no other packet-sending command moving them meanwhile.
    fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error>;
}

/// The wallet: the home's spending limit, its day's ledger, its packets lock and watermarks.
pub struct Wallet<'a> {
    home: &'a Path,
}

impl<'a> Wallet<'a> {
    pub fn new(home: &'a Path) -> Self {
        Wallet { home }
    }
}

impl Purse for Wallet<'_> {
    fn charge(&self, total: u128, yes: bool) -> Result<u64, Error> {
        spending::charge(self.home, total, yes)
    }

    fn refund(&self, token: u64, amount: u128) {
        spending::refund(self.home, token, amount);
    }

    fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error> {
        spending::packets_locked(self.home, |packets| act(&|cap| packets.moved(cap)))
    }
}

/// The packets of one command, as they are priced, and what they cost so far.
pub struct Packets<'a> {
    moved: &'a dyn Fn(u128) -> u128,
    /// What the packets sent so far cost, for the report.
    paid: Cell<u128>,
    /// What stays counted against the limit for them, which is more than `paid` when a
    /// packet failed after it may have left.
    counted: Cell<u128>,
}

impl Packets<'_> {
    /// What the packets sent so far cost.
    pub fn paid(&self) -> u128 {
        self.paid.get()
    }

    /// Send a packet of `amount` with `send` and price it. The report comes back with its
    /// `paid`, and the packet's cost beside it. An unanswered packet's error states what the
    /// command's packets cost, this one with them.
    pub fn send(
        &self,
        amount: u128,
        send: impl FnOnce() -> Result<Report, Error>,
    ) -> Result<(Report, u128), Error> {
        let mut report = send().map_err(|error| self.failed(amount, error))?;
        let paid = match report.json["outcome"].as_str() {
            Some("rejected" | "wrong_fulfilment") => self.moved_by(amount),
            _ => amount,
        };
        self.count(paid, paid);
        report.json["paid"] = json!(paid);
        Ok((report, paid))
    }

    /// Send a packet of `amount` with `send` and price it: the answer, and what the packet
    /// cost. An unanswered packet is an error that states what the command's packets cost,
    /// this one with them.
    pub fn answer(
        &self,
        amount: u128,
        send: impl FnOnce() -> Result<Answer, Error>,
    ) -> Result<(Answer, u128), Error> {
        let answer = send().map_err(|error| self.failed(amount, error))?;
        let paid = match answer {
            Answer::Fulfilled { .. } => amount,
            Answer::Rejected { .. } | Answer::WrongFulfilment | Answer::Unanswered => {
                self.moved_by(amount)
            }
        };
        self.count(paid, paid);
        if matches!(answer, Answer::Unanswered) {
            return Err(operator::unanswered_cost(self.paid(), None));
        }
        Ok((answer, paid))
    }

    /// What the watermarks moved by across a packet of `amount`, beyond what the packets
    /// before it cost, at most `amount`.
    fn moved_by(&self, amount: u128) -> u128 {
        let before = self.paid();
        (self.moved)(before + amount)
            .saturating_sub(before)
            .min(amount)
    }

    fn count(&self, paid: u128, counted: u128) {
        self.paid.set(self.paid() + paid);
        self.counted.set(self.counted.get() + counted);
    }

    /// Price a packet of `amount` whose send failed with `error`.
    fn failed(&self, amount: u128, error: Error) -> Error {
        if error.unanswered.is_some() {
            let moved = self.moved_by(amount);
            self.count(moved, moved);
            return operator::repriced(error, self.paid());
        }
        if !spending::failed_before_paying(&error) {
            // The money may have moved: it stays counted in full.
            self.count(self.moved_by(amount), amount);
        }
        error
    }
}

/// Pay for a command that sends packets: `total` is counted before `act` runs, and what its
/// packets did not cost is given back after, whatever `act` returns.
pub fn pay<P: Purse, T>(
    purse: &P,
    total: u128,
    yes: bool,
    act: impl FnOnce(&Packets) -> Result<T, Error>,
) -> Result<T, Error> {
    let token = purse.charge(total, yes)?;
    let locked = purse.locked(|moved| {
        let packets = Packets {
            moved,
            paid: Cell::new(0),
            counted: Cell::new(0),
        };
        let outcome = act(&packets);
        (outcome, packets.counted.get())
    });
    let (outcome, counted) = match locked {
        Ok(locked) => locked,
        Err(error) => (Err(error), 0),
    };
    let kept = counted.min(total);
    if kept < total {
        purse.refund(token, total - kept);
    }
    outcome
}

/// A purse for tests of a command's exit and `paid`: limits of its own, and watermarks that
/// move by what a test says.
#[cfg(test)]
pub mod fake {
    use std::cell::{Cell, RefCell};

    use super::*;
    use crate::outcome::ErrorCode;

    /// A purse with limits of its own and watermarks that move by what a test says.
    pub struct Fake {
        pub per_command: u128,
        pub remaining: Cell<u128>,
        pub moved: Cell<u128>,
        pub sent: RefCell<Vec<u128>>,
    }

    impl Fake {
        pub fn new() -> Self {
            Fake {
                per_command: 100,
                remaining: Cell::new(250),
                moved: Cell::new(0),
                sent: RefCell::new(Vec::new()),
            }
        }
    }

    impl Purse for Fake {
        fn charge(&self, total: u128, yes: bool) -> Result<u64, Error> {
            let refuse = |code, message: &str| Error {
                nothing_sent: false,
                unanswered: None,
                code,
                message: message.to_owned(),
            };
            if !yes {
                return Err(refuse(ErrorCode::NotConfirmed, "needs --yes"));
            }
            if total > self.per_command || total > self.remaining.get() {
                return Err(refuse(ErrorCode::SpendingLimit, "over the limit"));
            }
            self.remaining.set(self.remaining.get() - total);
            Ok(0)
        }

        fn refund(&self, _token: u64, amount: u128) {
            self.remaining.set(self.remaining.get() + amount);
        }

        fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error> {
            Ok(act(&|cap| self.moved.get().min(cap)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::Fake;
    use super::*;
    use crate::outcome::{ErrorCode, Exit};

    fn answered(outcome: &str) -> Result<Report, Error> {
        Ok(Report {
            exit: Exit::Success,
            json: json!({ "outcome": outcome }),
            text: String::new(),
        })
    }

    fn failure(code: ErrorCode, nothing_sent: bool) -> Error {
        Error {
            nothing_sent,
            unanswered: None,
            code,
            message: String::new(),
        }
    }

    /// Pay for one packet of 60 from `purse`, sending `send`: the report's `paid`, or the
    /// error.
    fn one(purse: &Fake, send: Result<Report, Error>) -> Result<serde_json::Value, Error> {
        pay(purse, 60, true, |packets| {
            purse.sent.borrow_mut().push(60);
            packets.send(60, || send).map(|(report, _)| report.json)
        })
    }

    #[test]
    fn a_fulfilled_packet_costs_its_amount() {
        let purse = Fake::new();
        let json = one(&purse, answered("fulfilled")).unwrap();
        assert_eq!(json["paid"], 60);
        assert_eq!(purse.remaining.get(), 190);
    }

    #[test]
    fn a_rejected_packet_costs_what_moved() {
        let purse = Fake::new();
        purse.moved.set(25);
        let json = one(&purse, answered("rejected")).unwrap();
        assert_eq!(json["paid"], 25);
        assert_eq!(purse.remaining.get(), 225);
    }

    #[test]
    fn a_rejected_packet_costs_at_most_its_amount() {
        let purse = Fake::new();
        purse.moved.set(500);
        assert_eq!(one(&purse, answered("rejected")).unwrap()["paid"], 60);
    }

    #[test]
    fn a_wrongly_fulfilled_packet_costs_what_moved() {
        let purse = Fake::new();
        purse.moved.set(10);
        let json = one(&purse, answered("wrong_fulfilment")).unwrap();
        assert_eq!(json["paid"], 10);
        assert_eq!(purse.remaining.get(), 240);
        purse.moved.set(500);
        assert_eq!(
            one(&purse, answered("wrong_fulfilment")).unwrap()["paid"],
            60
        );
    }

    #[test]
    fn an_unanswered_packet_costs_what_moved() {
        let purse = Fake::new();
        purse.moved.set(7);
        let error = one(&purse, Err(operator::unanswered_cost(60, None))).unwrap_err();
        assert_eq!(error.json()["paid"], 7);
        assert_eq!(purse.remaining.get(), 243);
    }

    #[test]
    fn a_failure_before_paying_costs_nothing_and_gives_the_amount_back() {
        let purse = Fake::new();
        purse.moved.set(60);
        let error = one(&purse, Err(failure(ErrorCode::SendFailed, true))).unwrap_err();
        assert_eq!(error.code, ErrorCode::SendFailed);
        assert_eq!(purse.remaining.get(), 250);
    }

    #[test]
    fn any_other_failure_stays_counted_in_full() {
        let purse = Fake::new();
        one(&purse, Err(failure(ErrorCode::SendFailed, false))).unwrap_err();
        assert_eq!(purse.remaining.get(), 190);
    }

    /// Pay 90 for packets of 30, each sent as `send` says, moving the fake's watermarks by
    /// what each one moved: the command's `paid`, or the error.
    fn three(purse: &Fake, sends: Vec<(u128, Result<Answer, Error>)>) -> Result<u128, Error> {
        pay(purse, 90, true, |packets| {
            for (moves, answer) in sends {
                packets.answer(30, || {
                    purse.moved.set(purse.moved.get() + moves);
                    answer
                })?;
            }
            Ok(packets.paid())
        })
    }

    fn fulfilled() -> Result<Answer, Error> {
        Ok(Answer::Fulfilled {
            status: 200,
            body: String::new(),
        })
    }

    #[test]
    fn packets_after_fulfilled_ones_cost_what_moved_beyond_them() {
        let purse = Fake::new();
        let rejected = Ok(Answer::Rejected {
            code: "F99".into(),
            message: String::new(),
            cost: 0,
        });
        let paid = three(
            &purse,
            vec![(30, fulfilled()), (30, fulfilled()), (4, rejected)],
        );
        assert_eq!(paid.unwrap(), 64);
        assert_eq!(purse.remaining.get(), 186);
    }

    #[test]
    fn an_unanswered_packet_after_fulfilled_ones_reports_what_all_of_them_cost() {
        let purse = Fake::new();
        let error = three(
            &purse,
            vec![
                (30, fulfilled()),
                (30, fulfilled()),
                (5, Ok(Answer::Unanswered)),
            ],
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SendFailed);
        assert_eq!(error.json()["paid"], 65);
        assert_eq!(purse.remaining.get(), 185);
    }

    #[test]
    fn a_failure_before_paying_keeps_what_earlier_packets_cost() {
        let purse = Fake::new();
        let error = three(
            &purse,
            vec![
                (30, fulfilled()),
                (0, Err(failure(ErrorCode::SendFailed, true))),
            ],
        )
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::SendFailed);
        assert_eq!(purse.remaining.get(), 220);
    }

    #[test]
    fn a_command_that_fails_before_its_first_packet_is_given_everything_back() {
        let purse = Fake::new();
        let error = pay(&purse, 60, true, |_| -> Result<(), Error> {
            Err(failure(ErrorCode::Io, false))
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert_eq!(purse.remaining.get(), 250);
    }

    #[test]
    fn nothing_is_sent_without_yes() {
        let purse = Fake::new();
        let error = pay(&purse, 60, false, |_| -> Result<(), Error> {
            purse.sent.borrow_mut().push(60);
            Ok(())
        })
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotConfirmed);
        assert!(purse.sent.borrow().is_empty());
        assert_eq!(purse.remaining.get(), 250);
    }

    #[test]
    fn nothing_is_sent_over_either_limit() {
        let purse = Fake::new();
        let refused = |purse: &Fake, total| {
            let error = pay(purse, total, true, |_| -> Result<(), Error> {
                purse.sent.borrow_mut().push(total);
                Ok(())
            })
            .unwrap_err();
            assert_eq!(error.code, ErrorCode::SpendingLimit);
        };
        refused(&purse, 101);
        purse.remaining.set(40);
        refused(&purse, 60);
        assert!(purse.sent.borrow().is_empty());
        assert_eq!(purse.remaining.get(), 40);
    }
}
