//! Picking a port for something that binds it later.
//!
//! A connector's port is picked by `toon init` and bound by every `toon up` after it, and
//! an app's ports are picked by the supervisor and bound by the app. Between the pick and
//! the bind the port is nobody's. The system hands out the ports of its ephemeral range by
//! itself, to every socket bound to port 0 and to every outgoing connection, so a port
//! picked in that range can be gone by the time it is bound: the connector then refuses to
//! start with "Address already in use". A port outside the range is taken only by a
//! program that asks for it by number.
//!
//! A port that is kept and a port that is bound at once come from either side of the
//! range, so the port an app is given is never one a connector of the same agent node
//! was promised and has not bound yet.
//!
//! A port that is bound at once is also claimed until it is bound, so that two supervisors
//! starting at the same moment are not given the same one. The claim is the UDP port of
//! the same number: holding it takes nothing from the TCP port, the system lets go of it
//! when its holder exits, however it exits, and no two processes can hold it at once.

use std::io;
use std::net::{TcpListener, UdpSocket};
use std::ops::RangeInclusive;

/// The lowest port that is picked. Below it are the ports that services are known by.
const LOWEST: u16 = 10_000;

/// How many ports are tried before the system is left to pick one.
const TRIES: usize = 64;

/// Where Linux says which ports it hands out by itself.
const EPHEMERAL_RANGE: &str = "/proc/sys/net/ipv4/ip_local_port_range";

/// The range in `text`, which is `ip_local_port_range`: the first and the last port.
fn parse(text: &str) -> Option<RangeInclusive<u16>> {
    let mut bounds = text.split_whitespace().map(str::parse::<u16>);
    match (bounds.next()?, bounds.next()?) {
        (Ok(first), Ok(last)) => Some(first..=last),
        _ => None,
    }
}

/// The ports the system hands out by itself. Where it does not say, every port from 32768
/// up: that covers Linux's default and the IANA range other systems use.
fn ephemeral() -> RangeInclusive<u16> {
    std::fs::read_to_string(EPHEMERAL_RANGE)
        .ok()
        .and_then(|text| parse(&text))
        .unwrap_or(32_768..=u16::MAX)
}

/// The ports from `LOWEST` up that are below `ephemeral`.
fn below(ephemeral: &RangeInclusive<u16>) -> Vec<u16> {
    (LOWEST..*ephemeral.start()).collect()
}

/// The ports that are above `ephemeral`.
fn above(ephemeral: &RangeInclusive<u16>) -> Vec<u16> {
    match ephemeral.end().checked_add(1) {
        Some(first) => (first.max(LOWEST)..=u16::MAX).collect(),
        None => Vec::new(),
    }
}

/// One of `candidates`, at random, so that two picks made at the same moment are not the
/// same port.
fn one_of(candidates: &[u16]) -> Option<u16> {
    let mut bytes = [0u8; 4];
    getrandom::getrandom(&mut bytes).ok()?;
    let index = u32::from_le_bytes(bytes) as usize % candidates.len().max(1);
    candidates.get(index).copied()
}

/// A port on `host` that is free now, out of `candidates`, and the claim on it if `claimed`
/// asks for one. A port that another process holds a claim on is not picked. If there are
/// no candidates, or none of the ones tried is free, the system picks: that port is free
/// only until something else takes it, and nobody holds a claim on it.
fn free(host: &str, candidates: &[u16], claimed: bool) -> io::Result<(u16, Option<UdpSocket>)> {
    for _ in 0..TRIES {
        let Some(port) = one_of(candidates) else {
            break;
        };
        let address = format!("{host}:{port}");
        let claim = match claimed.then(|| UdpSocket::bind(&address)).transpose() {
            Ok(claim) => claim,
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
            Err(_) => break,
        };
        match TcpListener::bind(&address) {
            Ok(_) => return Ok((port, claim)),
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            // Not a port that is taken: a host that cannot be bound at all.
            Err(_) => break,
        }
    }
    let port = TcpListener::bind(format!("{host}:0"))
        .and_then(|bound| bound.local_addr())
        .map(|address| address.port())?;
    Ok((port, None))
}

/// A port on `host` for what writes it down and binds it again each time it starts, as a
/// connector does: free now, and below the ports the system hands out by itself.
pub fn kept(host: &str) -> io::Result<u16> {
    free(host, &below(&ephemeral()), false).map(|(port, _)| port)
}

/// A port that is claimed for whoever was given it, until this is dropped.
pub struct Passing {
    pub port: u16,
    _claim: Option<UdpSocket>,
}

/// A port on `host` for what binds it at once and has another the next time it starts, as
/// an app does: free now, above the ports the system hands out by itself, and given to
/// nobody else for as long as what is returned is held, which is until the port is bound.
pub fn passing(host: &str) -> io::Result<Passing> {
    free(host, &above(&ephemeral()), true).map(|(port, claim)| Passing {
        port,
        _claim: claim,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_systems_range_is_read_as_its_first_and_last_port() {
        assert_eq!(parse("32768\t60999\n"), Some(32_768..=60_999));
        assert_eq!(parse("1024 65535"), Some(1_024..=65_535));
        assert_eq!(parse(""), None);
        assert_eq!(parse("32768"), None);
        assert_eq!(parse("low high"), None);
    }

    #[test]
    fn a_kept_port_and_a_passing_one_come_from_either_side_of_the_systems_range() {
        let ephemeral = 32_768..=60_999;

        let (below, above) = (below(&ephemeral), above(&ephemeral));

        assert_eq!(
            (below.first(), below.last()),
            (Some(&LOWEST), Some(&32_767))
        );
        assert_eq!(
            (above.first(), above.last()),
            (Some(&61_000), Some(&u16::MAX))
        );
    }

    #[test]
    fn a_range_that_leaves_no_port_leaves_the_system_to_pick() {
        let everything = 1_024..=u16::MAX;
        assert!(below(&everything).is_empty());
        assert!(above(&everything).is_empty());
        assert_eq!(one_of(&[]), None);

        let (port, claim) = free("127.0.0.1", &[], true).unwrap();

        assert_ne!(port, 0);
        assert!(claim.is_none());
    }

    #[test]
    fn a_picked_port_is_free_and_not_one_the_system_hands_out() {
        let passing = passing("127.0.0.1").unwrap();
        for port in [kept("127.0.0.1").unwrap(), passing.port] {
            // A system whose range leaves nothing on one side picks the port itself.
            let system = ephemeral();
            if !below(&system).is_empty() && !above(&system).is_empty() {
                assert!(
                    !system.contains(&port),
                    "{port} is the system's to hand out"
                );
                assert!(port >= LOWEST);
            }
            TcpListener::bind(("127.0.0.1", port)).expect("the picked port is free");
        }
    }

    #[test]
    fn a_port_that_is_taken_is_not_picked() {
        let taken = TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = taken.local_addr().unwrap().port();

        let (port, _) = free("127.0.0.1", &[taken], false).unwrap();

        assert_ne!(port, taken);
    }

    #[test]
    fn a_port_is_not_picked_again_while_it_is_claimed() {
        // The one candidate is a port that nothing is bound to.
        let (candidate, _) = free("127.0.0.1", &above(&ephemeral()), false).unwrap();

        let (first, claim) = free("127.0.0.1", &[candidate], true).unwrap();
        let (second, _) = free("127.0.0.1", &[candidate], true).unwrap();
        drop(claim);
        let (third, _) = free("127.0.0.1", &[candidate], true).unwrap();

        assert_eq!(first, candidate);
        assert_ne!(second, candidate, "it was claimed");
        assert_eq!(third, candidate, "the claim was let go");
    }

    #[test]
    fn a_host_that_cannot_be_bound_is_an_error() {
        assert!(kept("not a host").is_err());
    }
}
