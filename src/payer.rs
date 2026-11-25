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
//! A [`Purse`] is the seam behind the payer: the limit, the day's ledger, the watermarks and
//! the send. It has two adapters:
//!
//! - the wallet ([`Wallet`]), for a command run with the home, whose spending limit `toon`
//!   checks and counts;
//! - an agent key ([`AgentKey`]), for a command run with an agent key and no home (ADR 0010).
//!   Its limit is checked before the first packet, from what the connector says of it, and what
//!   moved is read from the connector's count. The connector counts and enforces the limit
//!   itself, so nothing is counted here.

use std::cell::Cell;
use std::path::Path;

use serde_json::json;

use crate::operator::{self, Answer, Surface};
use crate::outcome::{Error, ErrorCode, Report};
use crate::spending;

/// One packet a purse sends: a JSON `POST /` request carrying `body`.
pub struct Packet<'a> {
    pub destination: &'a str,
    pub amount: u64,
    /// The key it is sealed to: `None` seals it to the connector the purse sends through.
    pub seal_key: Option<&'a [u8; 65]>,
    pub body: Vec<u8>,
}

/// The limit, the ledger, the watermarks and the send a payer pays from.
pub trait Purse {
    /// Whether `total`, in packets of at most `largest` each, fits the limit, counting
    /// nothing.
    fn check(&self, total: u128, largest: u128) -> Result<(), Error>;
    /// Count `total`, in packets of at most `largest` each, against the limit, which needs
    /// `yes` and must fit. The token it returns is handed to [`Purse::refund`].
    fn charge(&self, total: u128, largest: u128, yes: bool) -> Result<u64, Error>;
    /// Give `amount` back to what [`Purse::charge`] counted it on.
    fn refund(&self, token: u64, amount: u128);
    /// Run `act` with what the outbound watermarks have moved by since, at most a cap given,
    /// with no other packet-sending command moving them meanwhile.
    fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error>;
    /// Send one packet and answer what came of it.
    fn send(&self, packet: Packet) -> Result<Answer, Error>;
}

fn json_headers() -> Vec<(String, String)> {
    vec![("content-type".to_owned(), "application/json".to_owned())]
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
    fn check(&self, total: u128, _largest: u128) -> Result<(), Error> {
        spending::check(self.home, total)
    }

    fn charge(&self, total: u128, _largest: u128, yes: bool) -> Result<u64, Error> {
        spending::charge(self.home, total, yes)
    }

    fn refund(&self, token: u64, amount: u128) {
        spending::refund(self.home, token, amount);
    }

    fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error> {
        spending::packets_locked(self.home, |packets| act(&|cap| packets.moved(cap)))
    }

    fn send(&self, packet: Packet) -> Result<Answer, Error> {
        let own;
        let key = match packet.seal_key {
            Some(key) => key,
            None => {
                own = operator::own_identity(self.home)?;
                &own
            }
        };
        operator::dispatch_with_headers(
            self.home,
            packet.destination,
            packet.amount,
            key,
            ("POST", "/"),
            json_headers(),
            packet.body,
        )
    }
}

/// What an agent key may still spend, as its connector states it: `None` states no limit.
#[derive(Clone, Copy, Debug, Default)]
pub struct AgentLimit {
    pub per_write: Option<u64>,
    pub remaining_today: Option<u64>,
    /// What the connector has counted against the key today, rejected packets in full.
    pub counted_today: Option<u64>,
}

/// The connector an agent key pays through: what it says of the key's limit, and the send.
pub trait KeyConnector {
    /// The key's limit, and what it has counted today, as the connector says now.
    fn limit(&self) -> Result<AgentLimit, Error>;
    /// Send one packet with the key.
    fn send(&self, packet: Packet) -> Result<Answer, Error>;
}

/// The connector at an operator surface reached with an agent key, whose public key lists it.
pub struct Connector {
    pub surface: Surface,
    /// The agent key's public key, as `GET /write-keys` lists it.
    pub public: String,
}

impl KeyConnector for Connector {
    fn limit(&self) -> Result<AgentLimit, Error> {
        let mine = operator::write_keys(&self.surface)?
            .into_iter()
            .find(|listed| {
                listed["keyid"]
                    .as_str()
                    .is_some_and(|id| id.eq_ignore_ascii_case(&self.public))
            })
            .ok_or_else(|| Error {
                nothing_sent: true,
                unanswered: None,
                code: ErrorCode::ConnectorFailed,
                message: format!(
                    "The connector of {} does not list this agent key.",
                    self.surface.toon_app()
                ),
            })?;
        let limit = &mine["spending_limit"];
        let per_day = limit["per_day"].as_u64();
        Ok(AgentLimit {
            per_write: limit["per_write"].as_u64(),
            remaining_today: per_day.map(|per_day| {
                per_day.saturating_sub(limit["counted_today"].as_u64().unwrap_or(0))
            }),
            counted_today: limit["counted_today"].as_u64(),
        })
    }

    fn send(&self, packet: Packet) -> Result<Answer, Error> {
        match packet.seal_key {
            Some(key) => operator::write_sealed_as_agent(
                &self.surface,
                packet.destination,
                packet.amount,
                key,
                packet.body,
            ),
            None => operator::write_as_agent(
                &self.surface,
                packet.destination,
                packet.amount,
                String::from_utf8_lossy(&packet.body).into_owned(),
            ),
        }
    }
}

/// An agent key with no home: its limit is the connector's, checked here before the first
/// packet, and what moved is what the connector counted against it.
pub struct AgentKey<C = Connector> {
    connector: C,
    /// What the connector had counted when the limit was charged: `None` when not read.
    before: Cell<Option<u64>>,
}

impl<C: KeyConnector> AgentKey<C> {
    pub fn new(connector: C) -> Self {
        AgentKey {
            connector,
            before: Cell::new(None),
        }
    }

    /// The key's limit, read from the connector, when `total` in packets of at most
    /// `largest` fits it. Nothing is read for a command that pays nothing.
    fn fits(&self, total: u128, largest: u128) -> Result<Option<AgentLimit>, Error> {
        if total == 0 {
            return Ok(None);
        }
        let limit = self.connector.limit()?;
        let refuse = |message: String| Error {
            nothing_sent: true,
            unanswered: None,
            code: ErrorCode::SpendingLimit,
            message,
        };
        if let Some(per_write) = limit.per_write.filter(|limit| largest > u128::from(*limit)) {
            return Err(refuse(format!(
                "One packet pays {largest} base units, which is over the agent key's \
                 per-write limit of {per_write} base units."
            )));
        }
        if let Some(remaining) = limit
            .remaining_today
            .filter(|left| total > u128::from(*left))
        {
            return Err(refuse(format!(
                "{total} base units is over what is left of the agent key's per-day limit, \
                 {remaining} base units."
            )));
        }
        Ok(Some(limit))
    }
}

impl<C: KeyConnector> Purse for AgentKey<C> {
    fn check(&self, total: u128, largest: u128) -> Result<(), Error> {
        self.fits(total, largest).map(|_| ())
    }

    fn charge(&self, total: u128, largest: u128, yes: bool) -> Result<u64, Error> {
        if !yes {
            return Err(Error {
                nothing_sent: true,
                unanswered: None,
                code: ErrorCode::NotConfirmed,
                message: format!(
                    "This moves {total} base units. Add `--yes` to say that you mean it."
                ),
            });
        }
        let limit = self.fits(total, largest)?;
        self.before.set(limit.and_then(|limit| limit.counted_today));
        Ok(0)
    }

    /// The connector counts what moved itself, and gives nothing back on the key's behalf.
    fn refund(&self, _token: u64, _amount: u128) {}

    fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error> {
        let before = self.before.get();
        // A count that cannot be read says the whole cap moved, as the wallet's watermarks do.
        Ok(act(&|cap| {
            let after = self
                .connector
                .limit()
                .ok()
                .and_then(|limit| limit.counted_today);
            match (before, after) {
                (Some(before), Some(after)) => after
                    .checked_sub(before)
                    .map_or(cap, |moved| u128::from(moved).min(cap)),
                _ => cap,
            }
        }))
    }

    fn send(&self, packet: Packet) -> Result<Answer, Error> {
        self.connector.send(packet)
    }
}

/// Either payer, for a command that is handed one or the other.
pub enum Payer<'a> {
    Wallet(Wallet<'a>),
    AgentKey(Box<AgentKey>),
}

impl Purse for Payer<'_> {
    fn check(&self, total: u128, largest: u128) -> Result<(), Error> {
        match self {
            Payer::Wallet(wallet) => wallet.check(total, largest),
            Payer::AgentKey(key) => key.check(total, largest),
        }
    }

    fn charge(&self, total: u128, largest: u128, yes: bool) -> Result<u64, Error> {
        match self {
            Payer::Wallet(wallet) => wallet.charge(total, largest, yes),
            Payer::AgentKey(key) => key.charge(total, largest, yes),
        }
    }

    fn refund(&self, token: u64, amount: u128) {
        match self {
            Payer::Wallet(wallet) => wallet.refund(token, amount),
            Payer::AgentKey(key) => key.refund(token, amount),
        }
    }

    fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error> {
        match self {
            Payer::Wallet(wallet) => wallet.locked(act),
            Payer::AgentKey(key) => key.locked(act),
        }
    }

    fn send(&self, packet: Packet) -> Result<Answer, Error> {
        match self {
            Payer::Wallet(wallet) => wallet.send(packet),
            Payer::AgentKey(key) => key.send(packet),
        }
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
        let (answer, paid) = self.priced(amount, send)?;
        if matches!(answer, Answer::Unanswered) {
            return Err(operator::unanswered_cost(self.paid(), None));
        }
        Ok((answer, paid))
    }

    /// [`Packets::answer`] for a command that reads an unanswered packet itself: it comes back
    /// as the answer, priced, and not as an error.
    pub fn priced(
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

/// Pay for a command that sends one packet, or packets that pay `total` between them: see
/// [`pay_packets`].
pub fn pay<P: Purse, T>(
    purse: &P,
    total: u128,
    yes: bool,
    act: impl FnOnce(&Packets) -> Result<T, Error>,
) -> Result<T, Error> {
    pay_packets(purse, total, total, yes, act)
}

/// Pay for a command that sends packets of at most `largest` each: `total` is counted before
/// `act` runs, and what its packets did not cost is given back after, whatever `act` returns.
pub fn pay_packets<P: Purse, T>(
    purse: &P,
    total: u128,
    largest: u128,
    yes: bool,
    act: impl FnOnce(&Packets) -> Result<T, Error>,
) -> Result<T, Error> {
    let token = purse.charge(total, largest, yes)?;
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
    use std::collections::VecDeque;

    use super::*;

    /// A purse with limits of its own and watermarks that move by what a test says.
    pub struct Fake {
        pub per_command: u128,
        pub remaining: Cell<u128>,
        pub moved: Cell<u128>,
        pub sent: RefCell<Vec<u128>>,
        /// What [`Purse::send`] answers, in order: fulfilled once these run out.
        pub answers: RefCell<VecDeque<Result<Answer, Error>>>,
    }

    impl Fake {
        pub fn new() -> Self {
            Fake {
                per_command: 100,
                remaining: Cell::new(250),
                moved: Cell::new(0),
                sent: RefCell::new(Vec::new()),
                answers: RefCell::new(VecDeque::new()),
            }
        }
    }

    fn refuse(code: ErrorCode, message: &str) -> Error {
        Error {
            nothing_sent: false,
            unanswered: None,
            code,
            message: message.to_owned(),
        }
    }

    impl Purse for Fake {
        fn check(&self, total: u128, _largest: u128) -> Result<(), Error> {
            if total > self.per_command || total > self.remaining.get() {
                return Err(refuse(ErrorCode::SpendingLimit, "over the limit"));
            }
            Ok(())
        }

        fn charge(&self, total: u128, largest: u128, yes: bool) -> Result<u64, Error> {
            if !yes {
                return Err(refuse(ErrorCode::NotConfirmed, "needs --yes"));
            }
            self.check(total, largest)?;
            self.remaining.set(self.remaining.get() - total);
            Ok(0)
        }

        fn refund(&self, _token: u64, amount: u128) {
            self.remaining.set(self.remaining.get() + amount);
        }

        fn locked<T>(&self, act: impl FnOnce(&dyn Fn(u128) -> u128) -> T) -> Result<T, Error> {
            Ok(act(&|cap| self.moved.get().min(cap)))
        }

        fn send(&self, packet: Packet) -> Result<Answer, Error> {
            self.sent.borrow_mut().push(packet.amount.into());
            self.answers
                .borrow_mut()
                .pop_front()
                .unwrap_or(Ok(Answer::Fulfilled {
                    status: 200,
                    body: String::new(),
                }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

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

    /// A connector that states `limit`, counts `counted` against the key once a packet is
    /// sent (or cannot say what it counted, with `None`), and answers `answer`.
    struct FakeConnector {
        limit: AgentLimit,
        counts: Cell<Option<u64>>,
        moves: Option<u64>,
        sent: RefCell<Vec<u64>>,
        answer: fn() -> Result<Answer, Error>,
    }

    impl FakeConnector {
        fn new(per_write: Option<u64>, remaining_today: Option<u64>) -> Self {
            FakeConnector {
                limit: AgentLimit {
                    per_write,
                    remaining_today,
                    counted_today: Some(500),
                },
                counts: Cell::new(Some(500)),
                moves: Some(0),
                sent: RefCell::new(Vec::new()),
                answer: || Ok(Answer::Unanswered),
            }
        }
    }

    impl KeyConnector for FakeConnector {
        fn limit(&self) -> Result<AgentLimit, Error> {
            Ok(AgentLimit {
                counted_today: self.counts.get(),
                ..self.limit
            })
        }

        fn send(&self, packet: Packet) -> Result<Answer, Error> {
            self.sent.borrow_mut().push(packet.amount);
            self.counts.set(
                self.moves
                    .and_then(|moves| Some(self.counts.get()? + moves)),
            );
            (self.answer)()
        }
    }

    /// Pay for one packet of `amount` with `key`: the packet's cost, or the error.
    fn one_with(key: &AgentKey<FakeConnector>, amount: u64) -> Result<u128, Error> {
        pay(key, amount.into(), true, |packets| {
            let sent = packets.answer(amount.into(), || {
                key.send(Packet {
                    destination: "g.somewhere",
                    amount,
                    seal_key: None,
                    body: Vec::new(),
                })
            });
            match sent {
                Ok((_, paid)) => Ok(paid),
                Err(error) => Err(error),
            }
        })
    }

    #[test]
    fn over_the_agent_keys_per_write_limit_nothing_is_sent() {
        let key = AgentKey::new(FakeConnector::new(Some(10), Some(1000)));
        let error = one_with(&key, 11).unwrap_err();
        assert_eq!(error.code, ErrorCode::SpendingLimit);
        assert!(error.nothing_sent);
        assert!(error.message.contains("11") && error.message.contains("10"));
        assert!(key.connector.sent.borrow().is_empty());
    }

    #[test]
    fn a_largest_packet_within_the_per_write_limit_is_sent_whatever_the_total() {
        let mut connector = FakeConnector::new(Some(10), Some(1000));
        connector.answer = fulfilled;
        let key = AgentKey::new(connector);
        let sent = pay_packets(&key, 30, 10, true, |packets| {
            for _ in 0..3 {
                packets.answer(10, || {
                    key.send(Packet {
                        destination: "g.somewhere",
                        amount: 10,
                        seal_key: None,
                        body: Vec::new(),
                    })
                })?;
            }
            Ok(())
        });
        assert!(sent.is_ok());
        assert_eq!(*key.connector.sent.borrow(), vec![10, 10, 10]);
    }

    #[test]
    fn over_what_is_left_of_the_agent_keys_day_nothing_is_sent() {
        let key = AgentKey::new(FakeConnector::new(Some(100), Some(40)));
        let error = one_with(&key, 60).unwrap_err();
        assert_eq!(error.code, ErrorCode::SpendingLimit);
        assert!(error.message.contains("60") && error.message.contains("40"));
        assert!(key.connector.sent.borrow().is_empty());
        assert_eq!(
            key.check(60, 60).unwrap_err().code,
            ErrorCode::SpendingLimit
        );
        assert!(key.check(40, 40).is_ok());
    }

    #[test]
    fn without_yes_an_agent_key_sends_nothing() {
        let key = AgentKey::new(FakeConnector::new(None, None));
        let error = pay(&key, 5, false, |_| -> Result<(), Error> { Ok(()) }).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotConfirmed);
    }

    #[test]
    fn what_moved_is_read_from_the_connectors_count() {
        let mut connector = FakeConnector::new(None, None);
        connector.moves = Some(7);
        let key = AgentKey::new(connector);
        let error = one_with(&key, 60).unwrap_err();
        assert_eq!(error.code, ErrorCode::SendFailed);
        assert_eq!(error.json()["paid"], 7);
    }

    #[test]
    fn what_moved_is_the_whole_cap_when_the_count_cannot_be_read() {
        let mut connector = FakeConnector::new(None, None);
        connector.moves = None;
        let key = AgentKey::new(connector);
        let error = one_with(&key, 60).unwrap_err();
        assert_eq!(error.json()["paid"], 60);
    }

    #[test]
    fn a_fulfilled_packet_of_an_agent_key_costs_its_amount() {
        let mut connector = FakeConnector::new(Some(100), Some(100));
        connector.answer = fulfilled;
        let key = AgentKey::new(connector);
        assert_eq!(one_with(&key, 60).unwrap(), 60);
    }
}
