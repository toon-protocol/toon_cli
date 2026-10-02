//! A peering toward an onion endpoint published seconds ago is repeated while the connector
//! cannot read the other side's self-description in time (its `FETCH_TIMEOUT`, 10 seconds).
//! The endpoint is a `.anyone` name that the loopback stand-in's proxy resolves, declared
//! with `TOON_OVERLAY_NAMES`, to a server that holds requests past that wait. Each held
//! request costs the test the connector's full 10 seconds.

mod support;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use support::anvil_chain::AnvilChain;
use support::fake_chain::FakeChain;
use support::{Foreground, Machine, Run};

const DEPOSIT: u128 = 1_000_000;
const NAME: &str = "slow.anyone";
const PORT: u16 = 7200;

/// How long a held request is held: past the connector's wait.
const HELD: Duration = Duration::from_secs(13);

/// What an endpoint does with the request at `index` (from 0).
#[derive(Clone, Copy)]
enum Answer {
    /// Hold it, then close.
    Hold,
    /// Answer with this status and no document.
    Status(u16),
    /// Pass it to the server at this address.
    Forward(SocketAddr),
}

/// An endpoint that does `answer(index)` to each connection, and counts them.
fn endpoint(answer: impl Fn(usize) -> Answer + Send + 'static) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("address");
    let seen = Arc::new(AtomicUsize::new(0));
    let counted = seen.clone();
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let index = counted.fetch_add(1, Ordering::SeqCst);
            let answer = answer(index);
            thread::spawn(move || serve(stream, answer));
        }
    });
    (address, seen)
}

fn serve(mut stream: TcpStream, answer: Answer) {
    match answer {
        Answer::Hold => {
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            thread::sleep(HELD);
        }
        Answer::Status(status) => {
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 {status} X\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
            );
        }
        Answer::Forward(target) => {
            let Ok(upstream) = TcpStream::connect(target) else {
                return;
            };
            let (mut from, mut to) = (
                stream.try_clone().expect("clone"),
                upstream.try_clone().expect("clone"),
            );
            thread::spawn(move || {
                let _ = std::io::copy(&mut from, &mut to);
            });
            let (mut upstream, mut stream) = (upstream, stream);
            let _ = std::io::copy(&mut upstream, &mut stream);
        }
    }
}

fn declared(address: SocketAddr) -> String {
    format!("{NAME}:{PORT}={address}")
}

fn toon(machine: &Machine, args: &[&str]) -> Run {
    machine.toon_with(args, |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    })
}

/// A hidden agent node, running, whose proxy knows `names`, with its init flags.
fn hidden_near(machine: &Machine, init: Run, names: &str) -> Foreground {
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    assert_eq!(init.json()["toon_apps"][0]["reach"], "hidden");
    start(machine, names)
}

fn start(machine: &Machine, names: &str) -> Foreground {
    let up = machine.start_with(&["up", "--foreground", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        command.env("TOON_OVERLAY_NAMES", names);
    });
    up.report();
    up
}

fn peer_add(machine: &Machine) -> Run {
    toon(
        machine,
        &[
            "peer",
            "add",
            &format!("http://{NAME}:{PORT}/ilp"),
            "--deposit",
            &DEPOSIT.to_string(),
            "--yes",
            "--id",
            "far",
            "--json",
        ],
    )
}

/// A clearnet agent node on `chain`, running, funded.
fn far_on(chain: &AnvilChain) -> (Machine, Foreground, SocketAddr) {
    let machine = Machine::new();
    let init = machine.init_on_anvil(chain, true);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let evm = toon(&machine, &["wallet", "show", "--json"]).json()["wallet"]["chains"]["evm"][0]
        ["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    let status = machine.toon(&["status", "--json"]).json();
    let address = status["agent_node"]["toon_apps"][0]["connector"]["address"]
        .as_str()
        .expect("the connector's address")
        .parse()
        .expect("a socket address");
    (machine, up, address)
}

#[test]
fn a_peering_whose_first_read_outlasts_the_connectors_wait_is_made_by_the_next() {
    let chain = AnvilChain::start();
    let (_far, _far_up, far_address) = far_on(&chain);
    let (address, seen) = endpoint(move |index| {
        if index == 0 {
            Answer::Hold
        } else {
            Answer::Forward(far_address)
        }
    });
    let machine = Machine::new();
    let init = machine.init_with(&[
        "--evm-rpc-url",
        &chain.rpc_url(),
        "--evm-token",
        &chain.token(),
        "--evm-decimals",
        &support::anvil_chain::TOKEN_DECIMALS.to_string(),
        "--allow-plaintext-peers",
    ]);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let evm = toon(&machine, &["wallet", "show", "--json"]).json()["wallet"]["chains"]["evm"][0]
        ["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let _up = start(&machine, &declared(address));

    let run = peer_add(&machine);

    assert_eq!(run.exit_code, 0, "{}{}", run.stdout, run.stderr);
    let json = run.json();
    assert_eq!(json["deposited"], true, "{}", run.stdout);
    assert_eq!(json["peering"]["id"], "far", "{}", run.stdout);
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[test]
fn an_endpoint_that_never_answers_is_attempted_four_times_and_the_refusal_is_reported() {
    let chain = FakeChain::start();
    let (address, seen) = endpoint(|_| Answer::Hold);
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    let _up = hidden_near(&machine, init, &declared(address));

    let run = peer_add(&machine);

    assert_eq!(run.exit_code, 1, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.json()["error"]["code"], "peer_failed", "{}", run.stdout);
    assert!(
        run.stdout.contains("could not read the self-description"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("timed out"), "{}", run.stdout);
    assert_eq!(seen.load(Ordering::SeqCst), 4);
}

#[test]
fn a_refusal_that_is_not_a_timeout_is_attempted_once() {
    let chain = FakeChain::start();
    let (address, seen) = endpoint(|_| Answer::Status(500));
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    let _up = hidden_near(&machine, init, &declared(address));

    let started = Instant::now();
    let run = peer_add(&machine);

    assert_eq!(run.json()["error"]["code"], "peer_failed", "{}", run.stdout);
    assert!(run.stdout.contains("500"), "{}", run.stdout);
    assert_eq!(seen.load(Ordering::SeqCst), 1);
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_refused_connection_is_attempted_once() {
    let chain = FakeChain::start();
    let closed = TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("address");
    let machine = Machine::new();
    let init = machine.init_on(&chain);
    let _up = hidden_near(&machine, init, &declared(closed));

    let started = Instant::now();
    let run = peer_add(&machine);

    assert_eq!(run.json()["error"]["code"], "peer_failed", "{}", run.stdout);
    assert!(!run.stdout.contains("timed out"), "{}", run.stdout);
    assert!(started.elapsed() < Duration::from_secs(10));
}
