//! The payer: what pays for a command's packets, and decides what each one cost.
//!
//! A command gives [`pay`] the total it will pay and a closure that sends its packets. Each
//! packet goes through [`Packets::send`], which prices it by one rule:
//!
//! - fulfilled with the right fulfilment: its amount;
//! - rejected, fulfilled with the wrong fulfilment, or unanswered: what the outbound
//!   watermarks moved by across it, at most its amount;
//! - failed certainly before paying: nothing;
//! - any other failure: stays counted in full against the limit.
//!
//! The payer settles the limit once, giving back what did not move. The command decides what
//! to send and when to stop, and writes its own report text.
//!
//! The only payer today is the wallet, whose spending limit `toon` checks ([`Wallet`]). A
//! [`Purse`] is the seam behind it: the limit, the day's ledger and the watermarks.

use std::path::Path;

use serde_json::json;

use crate::operator;
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

/// The packets of one command, as they are priced.
pub struct Packets<'a> {
    moved: &'a dyn Fn(u128) -> u128,
}

impl Packets<'_> {
    /// Send a packet of `amount` with `send` and price it. The report comes back with its
    /// `paid`; an unanswered packet's error is priced by what moved.
    pub fn send(
        &self,
        amount: u128,
        send: impl FnOnce() -> Result<Report, Error>,
    ) -> Result<(Report, u128), Error> {
        let mut report = send().map_err(|error| operator::repriced(error, (self.moved)(amount)))?;
        let paid = match report.json["outcome"].as_str() {
            Some("rejected" | "wrong_fulfilment") => (self.moved)(amount),
            _ => amount,
        };
        report.json["paid"] = json!(paid);
        Ok((report, paid))
    }
}

/// Pay for a command that sends packets: `total` is counted before `act` runs and what did
/// not move is given back after. `act` hands back its report and what its packets cost.
pub fn pay<P: Purse, T>(
    purse: &P,
    total: u128,
    yes: bool,
    act: impl FnOnce(&Packets) -> Result<(T, u128), Error>,
) -> Result<T, Error> {
    let token = purse.charge(total, yes)?;
    let outcome = purse.locked(|moved| act(&Packets { moved }))?;
    let kept = spending::kept(&outcome, total);
    if kept < total {
        purse.refund(token, total - kept);
    }
    outcome.map(|(report, _)| report)
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
            packets
                .send(60, || send)
                .map(|(report, paid)| (report.json, paid))
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

    #[test]
    fn nothing_is_sent_without_yes() {
        let purse = Fake::new();
        let error = pay(&purse, 60, false, |_| -> Result<((), u128), Error> {
            purse.sent.borrow_mut().push(60);
            Ok(((), 0))
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
            let error = pay(purse, total, true, |_| -> Result<((), u128), Error> {
                purse.sent.borrow_mut().push(total);
                Ok(((), 0))
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
