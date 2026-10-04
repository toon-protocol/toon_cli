mod support;

use support::anvil_chain::AnvilChain;
use support::{Foreground, Machine};

/// What the channel toward the network's connector is opened with: one USDC.
const DEPOSIT: u128 = 1_000_000;

struct Node {
    machine: Machine,
    _up: Foreground,
    address: String,
    evm: String,
}

/// An agent node that joins a network whose connector is `network`'s, if there is one.
fn node_on(chain: &AnvilChain, network: Option<(&str, &str)>) -> Node {
    match network {
        Some((connector, relay)) => node_with(
            chain,
            &[
                "--network",
                "devnet",
                "--connector-url",
                connector,
                "--relay-url",
                relay,
            ],
        ),
        None => node_with(chain, &["--network", "devnet"]),
    }
}

/// An agent node initialised with `network_args` on top of the settings for `chain`.
fn node_with(chain: &AnvilChain, network_args: &[&str]) -> Node {
    let machine = Machine::new();
    let rpc_url = chain.rpc_url();
    let token = chain.token();
    let decimals = support::anvil_chain::TOKEN_DECIMALS.to_string();
    let mut args = network_args.to_vec();
    args.extend([
        "--clearnet",
        "toon.example.com",
        "--evm-rpc-url",
        &rpc_url,
        "--evm-token",
        &token,
        "--evm-decimals",
        &decimals,
        "--allow-plaintext-peers",
    ]);
    let init = machine.init_with(&args);
    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let shown = machine.toon_with(&["wallet", "show", "--json"], |command| {
        command.env("TOON_PASSPHRASE", support::PASSPHRASE);
    });
    let evm = shown.json()["wallet"]["chains"]["evm"][0]["address"]
        .as_str()
        .expect("the wallet's EVM address")
        .to_owned();
    chain.fund(&evm, DEPOSIT * 10);
    let up = machine.start(&["up", "--foreground", "--json"]);
    let address = up.report()["connector"]["address"]
        .as_str()
        .expect("the connector's address")
        .to_owned();
    Node {
        machine,
        _up: up,
        address,
        evm,
    }
}

#[test]
fn a_new_agent_node_is_unconnected_until_it_joins_and_then_reads_the_networks_relay() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let before = agent.machine.toon(&["status"]);
    assert!(before.stdout.contains("Unconnected"), "{}", before.stdout);
    let peers = agent.machine.toon(&["peer", "list", "--json"]);
    assert_eq!(peers.json()["peers"].as_array().map(Vec::len), Some(0));
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 10, "nothing is spent");

    let unconfirmed = agent
        .machine
        .toon(&["join", "devnet", "--deposit", &DEPOSIT.to_string()]);
    assert_eq!(unconfirmed.exit_code, 1);
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 10);

    let joined = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);
    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert_eq!(joined.json()["relay"], "ws://127.0.0.1:7100");
    assert_eq!(
        chain.balance(&agent.evm),
        DEPOSIT * 9,
        "the deposit is on chain"
    );

    let peers = agent.machine.toon(&["peer", "list", "--json"]);
    assert_eq!(peers.json()["peers"][0]["id"], "devnet");
    let routes = agent.machine.toon(&["route", "list"]);
    assert!(routes.stdout.contains("g.toon"), "{}", routes.stdout);

    let after = agent.machine.toon(&["status", "--json"]);
    assert_eq!(
        after.json()["agent_node"]["reads"][0],
        "ws://127.0.0.1:7100"
    );
    assert_eq!(after.json()["agent_node"]["joined"], "devnet");

    let left = agent.machine.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"]
        .clone();
    let again = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);
    assert_eq!(again.json()["error"]["code"], "join_refused");
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 9);
    let still = agent.machine.toon(&["limit", "show", "--json"]);
    assert_eq!(
        still.json()["limits"]["remaining_today"],
        left,
        "a refused join is not counted"
    );
}

#[test]
fn join_over_the_per_command_limit_is_refused_and_spends_nothing() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let joined = agent.machine.toon(&[
        "join",
        "devnet",
        "--deposit",
        "999999999999",
        "--yes",
        "--json",
    ]);
    assert_eq!(joined.json()["error"]["code"], "spending_limit");
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 10);
}

#[test]
fn a_mainnet_agent_node_is_joined_through_the_connector_named_at_init() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    // The connector and relay are named at `init`, so the real mainnet node is not contacted;
    // the anvil token names itself as the devnet's does.
    let agent = node_with(
        &chain,
        &[
            "--network",
            "mainnet",
            "--connector-url",
            &connector,
            "--relay-url",
            "ws://127.0.0.1:7100",
            "--evm-asset-name",
            "USDC",
        ],
    );

    let joined = agent.machine.toon(&[
        "join",
        "mainnet",
        "--deposit",
        &DEPOSIT.to_string(),
        "--yes",
        "--json",
    ]);

    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert_eq!(joined.json()["relay"], "ws://127.0.0.1:7100");
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 9);
    let peers = agent.machine.toon(&["peer", "list", "--json"]);
    assert_eq!(peers.json()["peers"][0]["id"], "mainnet");
    let after = agent.machine.toon(&["status", "--json"]);
    assert_eq!(after.json()["agent_node"]["joined"], "mainnet");
    assert_eq!(
        after.json()["agent_node"]["reads"].as_array().map(Vec::len),
        Some(1)
    );
}

#[test]
fn a_join_that_finds_its_channel_open_deposits_nothing_and_is_not_counted() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));
    let deposit = DEPOSIT.to_string();

    // An earlier peering under the network's label opened the channel.
    let peered = agent.machine.toon(&[
        "peer",
        "add",
        &connector,
        "--deposit",
        &deposit,
        "--yes",
        "--id",
        "devnet",
        "--json",
    ]);
    assert_eq!(peered.exit_code, 0, "{}", peered.stdout);
    let balance = chain.balance(&agent.evm);
    let remaining = |agent: &Node| {
        agent.machine.toon(&["limit", "show", "--json"]).json()["limits"]["remaining_today"].clone()
    };
    let left = remaining(&agent);

    std::thread::sleep(std::time::Duration::from_secs(2));
    let joined = agent
        .machine
        .toon(&["join", "devnet", "--deposit", &deposit, "--yes", "--json"]);
    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    assert_eq!(joined.json()["peering"]["channel"]["status"], "found");
    assert_eq!(joined.json()["deposited"], false);
    assert_eq!(chain.balance(&agent.evm), balance, "nothing was deposited");
    assert_eq!(remaining(&agent), left, "and nothing is counted");
}

/// An app of `network` at `address`, served by a listener that answers every request.
fn app_at(network: &Node, address: &str) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/inbox", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        use std::io::{Read, Write};
        for mut stream in listener.incoming().flatten() {
            let mut buffer = [0u8; 8192];
            let _ = stream.read(&mut buffer);
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        }
    });
    let name = address.replace('.', "-");
    let added = network.machine.toon(&[
        "add",
        &name,
        "--to",
        "relay",
        "--url",
        &url,
        "--address",
        address,
        "--yes",
        "--json",
    ]);
    assert_eq!(added.exit_code, 0, "{}", added.stdout);
}

fn join_report(agent: &Node) -> support::Run {
    join_as(agent, &["--json"])
}

fn join_as(agent: &Node, format: &[&str]) -> support::Run {
    let mut args = vec!["join", "devnet", "--deposit", "1000000", "--yes"];
    args.extend_from_slice(format);
    let joined = agent.machine.toon(&args);
    assert_eq!(joined.exit_code, 0, "{}", joined.stdout);
    joined
}

fn forwarding_prefixes(agent: &Node) -> Vec<String> {
    let routes = agent.machine.toon(&["route", "list", "--json"]).json();
    let mut prefixes: Vec<String> = routes["forwarding_routes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|route| {
            assert_eq!(route["peer_id"], "devnet", "{route}");
            assert_eq!(route["price"], 0, "{route}");
            route["prefix"].as_str().unwrap().to_owned()
        })
        .collect();
    prefixes.sort();
    prefixes
}

#[test]
fn a_join_forwards_the_addresses_the_connector_publishes_outside_the_network_prefix() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    app_at(&network, "g.drew.inbox");
    app_at(&network, "g.drew.inbox.quote");
    let connector = publishing_also(
        &network.address,
        Some(&["g.drew.inbox.quote", "g.drew.inbox", "g.toon.x.relay"]),
    );
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let joined = join_report(&agent);

    assert_eq!(joined.json()["route"]["prefix"], "g.toon");
    assert_eq!(
        joined.json()["routes"],
        serde_json::json!(["g.toon", "g.drew.inbox"]),
        "{}",
        joined.stdout
    );
    assert_eq!(
        forwarding_prefixes(&agent),
        ["g.drew.inbox", "g.toon"],
        "g.drew.inbox.quote is covered by g.drew.inbox"
    );
    // The network's app at an address outside `g.toon` is paid with no `route add`.
    let relay = relay_document(&format!("http://{}/ilp", network.address), "g.drew.inbox");
    let published = agent.machine.toon_with(
        &[
            "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", support::PASSPHRASE);
        },
    );
    assert_eq!(
        published.exit_code, 0,
        "{}{}",
        published.stdout, published.stderr
    );
}

/// A relay's information document naming the connector at `url` as where `address` is paid.
/// Returns the relay's `ws://` URL.
fn relay_document(url: &str, address: &str) -> String {
    use std::io::{Read, Write};
    let url = url.to_owned();
    let body = serde_json::json!({ "name": "relay", "toon": {
        "ilp_address": address,
        "connector_url": url,
        "connector_seal_key": support::seal_key(&url),
        "price": 0,
    }})
    .to_string();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let at = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for mut stream in listener.incoming().flatten() {
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/nostr+json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    format!("ws://{at}")
}

#[test]
fn a_join_that_cannot_read_the_self_description_ends_joined_with_the_network_prefix() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = publishing_also(&network.address, None);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let joined = join_as(&agent, &[]);

    assert_eq!(forwarding_prefixes(&agent), ["g.toon"]);
    assert!(
        joined.stdout.contains("were not routed")
            && joined
                .stdout
                .contains("toon route add <address> --peer devnet"),
        "{}",
        joined.stdout
    );
    assert_eq!(chain.balance(&agent.evm), DEPOSIT * 9, "the deposit stands");
}

#[test]
fn a_join_whose_connector_publishes_only_the_network_prefix_adds_one_route() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    let connector = format!("http://{}/ilp", network.address);
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let joined = join_as(&agent, &[]);

    assert!(
        joined.stdout.contains("forwarding g.toon to it"),
        "{}",
        joined.stdout
    );
    assert_eq!(forwarding_prefixes(&agent), ["g.toon"]);
}

#[test]
fn a_joined_node_asked_to_pay_an_unrouted_address_of_its_network_is_asked_only_for_a_route() {
    let chain = AnvilChain::start();
    let network = node_on(&chain, None);
    app_at(&network, "g.drew.inbox");
    let connector = publishing_also(&network.address, Some(&["g.drew.inbox"]));
    let agent = node_on(&chain, Some((&connector, "ws://127.0.0.1:7100")));

    let joined = join_as(&agent, &[]);

    assert!(
        joined
            .stdout
            .contains("forwarding g.toon, g.drew.inbox to it"),
        "{}",
        joined.stdout
    );
    // The relay names the connector the node joined, at an address no route forwards.
    let relay = relay_document(&connector, "g.other.relay");
    let published = agent.machine.toon(&[
        "event", "publish", "--relay", &relay, "--kind", "1", "--yes", "--json",
    ]);
    let error = published.json()["error"].clone();
    assert_eq!(error["code"], "peering_needed", "{error}");
    assert_eq!(published.exit_code, 1);
    let message = error["message"].as_str().unwrap();
    assert!(
        message.contains("toon route add g.other.relay --peer devnet"),
        "{message}"
    );
    assert!(!message.contains("peer add"), "{message}");
    assert!(!message.contains("--deposit"), "{message}");
}

/// A listener in front of the connector at `upstream` that passes every request through,
/// and adds `extra` to the `ilpAddresses` of its self-description. Returns its `/ilp` URL.
fn publishing_also(upstream: &str, extra: Option<&[&str]>) -> String {
    use std::io::{Read, Write};
    let upstream = upstream.to_owned();
    let described = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let extra: Option<Vec<String>> =
        extra.map(|extra| extra.iter().map(|address| address.to_string()).collect());
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/ilp", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        for client in listener.incoming().flatten() {
            let (upstream, extra) = (upstream.clone(), extra.clone());
            let described = described.clone();
            std::thread::spawn(move || {
                let mut client = client;
                let mut request = Vec::new();
                let mut chunk = [0u8; 8192];
                let end = loop {
                    let read = client.read(&mut chunk).unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    request.extend_from_slice(&chunk[..read]);
                    if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..at]).to_lowercase();
                        let length = head
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if request.len() >= at + 4 + length {
                            break at;
                        }
                    }
                };
                let head = String::from_utf8_lossy(&request[..end]).into_owned();
                let is_description = head.starts_with("GET /ilp ");
                // HTTP/1.0 keeps the connector from chunking its answer.
                let head = head.replacen("HTTP/1.1", "HTTP/1.0", 1);
                let mut onward = std::net::TcpStream::connect(&upstream).unwrap();
                onward.write_all(head.as_bytes()).unwrap();
                onward.write_all(&request[end..]).unwrap();
                let mut answer = Vec::new();
                onward.read_to_end(&mut answer).unwrap();
                let at = answer.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
                // The peering reads the description once; a later read fails when `extra` is None.
                let later = is_description
                    && described.fetch_add(1, std::sync::atomic::Ordering::SeqCst) > 0;
                let reply = if later && extra.is_none() {
                    b"HTTP/1.1 500 Oops\r\ncontent-length: 0\r\nconnection: close\r\n\r\n".to_vec()
                } else if is_description && extra.is_some() {
                    let extra = extra.unwrap();
                    let mut document: serde_json::Value =
                        serde_json::from_slice(&answer[at + 4..]).unwrap();
                    let listed = document["ilpAddresses"].as_array_mut().unwrap();
                    listed.extend(extra.iter().map(|address| address.clone().into()));
                    let body = document.to_string();
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .into_bytes()
                } else {
                    answer
                };
                let _ = client.write_all(&reply);
            });
        }
    });
    url
}
