mod support;

use std::fs;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use support::fake_chain::FakeChain;
use support::{Machine, PASSPHRASE};

fn config(machine: &Machine) -> String {
    fs::read_to_string(
        machine
            .agent_node_home()
            .join("connectors/0/connector.toml"),
    )
    .expect("read the connector's config")
}

/// The value of the top-level or table key `name` in the rendered config, unquoted.
fn value(config: &str, name: &str) -> String {
    let line = config
        .lines()
        .find(|line| line.starts_with(&format!("{name} = ")))
        .unwrap_or_else(|| panic!("no {name} in\n{config}"));
    line.split_once(" = ")
        .unwrap()
        .1
        .trim_matches(['"', '[', ']'])
        .to_owned()
}

fn onion_endpoint(config: &str) -> String {
    let url = value(config, "http_endpoint");
    url.strip_prefix("http://")
        .and_then(|rest| rest.strip_suffix("/ilp"))
        .unwrap_or_else(|| panic!("an unexpected http_endpoint: {url}"))
        .to_owned()
}

fn proxy(config: &str) -> SocketAddr {
    value(config, "socks_proxy")
        .strip_prefix("socks5h://")
        .and_then(|address| address.parse().ok())
        .unwrap_or_else(|| panic!("an unexpected socks_proxy in\n{config}"))
}

/// `GET /health` at `host:port` through the SOCKS proxy, by name as `socks5h` does.
fn health_through(proxy: SocketAddr, host: &str, port: u16) -> Option<String> {
    let mut stream = TcpStream::connect(proxy).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    stream.write_all(&[5, 1, 0]).unwrap();
    let mut method = [0u8; 2];
    stream.read_exact(&mut method).unwrap();
    let mut request = vec![5, 1, 0, 3, host.len() as u8];
    request.extend_from_slice(host.as_bytes());
    request.extend_from_slice(&port.to_be_bytes());
    stream.write_all(&request).unwrap();
    let mut reply = [0u8; 10];
    stream.read_exact(&mut reply).unwrap();
    if reply[1] != 0 {
        return None;
    }
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .unwrap();
    let mut answer = String::new();
    stream.read_to_string(&mut answer).ok()?;
    Some(answer)
}

#[test]
fn a_new_toon_app_is_a_hidden_service() {
    let chain = FakeChain::start();
    let machine = Machine::new();

    let init = machine.init_on(&chain);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let config = config(&machine);
    let endpoint = onion_endpoint(&config);
    assert!(endpoint.ends_with(".anyone"), "{endpoint}");
    // No clearnet listener: the connector binds loopback, and the onion endpoint is the
    // address it is published at on every carriage.
    let listen: SocketAddr = value(&config, "client_edge_addr").parse().unwrap();
    assert!(listen.ip().is_loopback(), "{config}");
    assert_eq!(
        value(&config, "btp_endpoint"),
        format!("ws://{endpoint}/ilp/btp")
    );
    // Outbound traffic goes through the overlay's proxy, which is on this machine.
    assert!(proxy(&config).ip().is_loopback());
    let report = init.json();
    let app = &report["toon_apps"][0];
    assert_eq!(app["reach"], "hidden");
    assert_eq!(app["onion_endpoint"], endpoint);
    assert_eq!(app["ports"]["connector"], 80);
    assert_eq!(app["ports"]["relay_read"], 7100);
    assert!(machine
        .agent_node_home()
        .join("overlay/0/hostname")
        .exists());
}

#[test]
fn settlement_rpc_goes_through_the_proxy() {
    let machine = Machine::new();

    let init = machine.init_with(&["--solana", "--evm-rpc-url", "https://rpc.example/evm"]);

    assert_eq!(init.exit_code, 0, "{}", init.stdout);
    let config = config(&machine);
    assert_eq!(
        config.matches("rpc_via_socks_proxy = true").count(),
        2,
        "{config}"
    );
}

#[test]
fn the_output_says_what_a_hidden_service_hides_and_what_it_does_not() {
    let chain = FakeChain::start();
    let machine = Machine::new();

    let init = machine.init_on(&chain);

    let note = "hides where the TOON app is reachable, and not who it pays";
    assert!(init.json()["notes"][0].as_str().unwrap().contains(note));
    let other = Machine::new();
    let text = other.toon_with(
        &[
            "init",
            "--accept-anyone-terms",
            "--evm-rpc-url",
            &chain.rpc_url(),
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
        },
    );
    assert!(text.stdout.contains(note), "{}", text.stdout);
    assert!(text.stdout.contains(".anyone"), "{}", text.stdout);
}

#[test]
fn the_relays_read_port_and_the_connector_are_one_address_on_two_ports() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let endpoint = onion_endpoint(&config(&machine));

    let up = machine.start(&["up", "--foreground", "--json"]);
    up.report();

    let proxy = proxy(&config(&machine));
    let read = health_through(proxy, &endpoint, 7100).expect("the read port is published");
    assert!(read.starts_with("HTTP/1.1 200"), "{read}");
    let connector = health_through(proxy, &endpoint, 80).expect("the connector is published");
    assert!(connector.starts_with("HTTP/1.1"), "{connector}");
    assert!(health_through(proxy, &endpoint, 81).is_none());
}

#[test]
fn an_onion_endpoint_is_the_same_after_a_restart() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);
    let before = onion_endpoint(&config(&machine));
    let key = fs::read(machine.agent_node_home().join("connectors/0/onion.key")).unwrap();

    let mut up = machine.start(&["up", "--foreground", "--json"]);
    up.report();
    assert_eq!(onion_endpoint(&config(&machine)), before);
    assert_eq!(machine.toon(&["down", "--json"]).exit_code, 0);
    assert_eq!(up.exit_code(), 0);
    let again = machine.start(&["up", "--foreground", "--json"]);
    again.report();

    assert_eq!(onion_endpoint(&config(&machine)), before);
    assert_eq!(
        fs::read(machine.agent_node_home().join("connectors/0/onion.key")).unwrap(),
        key
    );
}

#[test]
fn two_wallets_have_two_onion_endpoints() {
    let chain = FakeChain::start();
    let (one, two) = (Machine::new(), Machine::new());
    one.init_on(&chain);
    two.init_on(&chain);

    assert_ne!(onion_endpoint(&config(&one)), onion_endpoint(&config(&two)));
}

fn leaves_nothing(machine: &Machine) {
    let home = machine.agent_node_home();
    for file in [
        "keystore.json",
        "state.json",
        "connectors",
        "apps",
        "overlay",
    ] {
        assert!(!home.join(file).exists(), "{file} was left behind");
    }
}

#[test]
fn init_fails_and_leaves_nothing_when_the_overlay_cannot_bootstrap() {
    let machine = Machine::new();

    for overlay in [None, Some("anon")] {
        let run = machine.toon_with(&["init", "--json", "--accept-anyone-terms"], |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
            match overlay {
                Some(name) => command.env("TOON_OVERLAY", name),
                None => command.env_remove("TOON_OVERLAY"),
            };
        });

        assert_eq!(run.exit_code, 1, "{}", run.stdout);
        let error = &run.json()["error"];
        assert_eq!(error["code"], "overlay_unavailable");
        assert!(error["message"].as_str().unwrap().contains("clearnet"));
        leaves_nothing(&machine);
    }
}

#[test]
fn up_does_not_fall_back_to_clearnet_when_the_overlay_is_gone() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    machine.init_on(&chain);

    let run = machine.toon_with(&["up", "--foreground", "--json"], |command| {
        command.env_remove("TOON_OVERLAY");
    });

    assert_eq!(run.json()["error"]["code"], "overlay_unavailable");
    assert_eq!(run.exit_code, 1);
    assert!(!machine.agent_node_home().join("supervisor.sock").exists());
}

#[test]
fn init_needs_the_operator_to_agree_to_the_anyone_terms() {
    let machine = Machine::new();

    let run = machine.toon_with(&["init", "--json"], |command| {
        command.env("TOON_PASSPHRASE", PASSPHRASE);
    });

    assert_eq!(run.exit_code, 2, "{}", run.stdout);
    let error = &run.json()["error"];
    assert_eq!(error["code"], "usage");
    assert!(error["message"]
        .as_str()
        .unwrap()
        .contains("--accept-anyone-terms"));
    leaves_nothing(&machine);
}

#[test]
fn a_hidden_service_does_not_listen_on_a_public_address() {
    let machine = Machine::new();

    let run = machine.init_with(&["--listen", "0.0.0.0:4000"]);

    assert_eq!(run.exit_code, 2, "{}", run.stdout);
    assert_eq!(run.json()["error"]["code"], "usage");
    leaves_nothing(&machine);
}

#[test]
fn clearnet_is_asked_for_with_a_hostname_and_binds_a_local_port() {
    let machine = Machine::new();

    // No terms to agree to and no overlay needed.
    let run = machine.toon_with(
        &[
            "init",
            "--clearnet",
            "toon.example.com",
            "--listen",
            "0.0.0.0:4000",
        ],
        |command| {
            command.env("TOON_PASSPHRASE", PASSPHRASE);
            command.env_remove("TOON_OVERLAY");
        },
    );

    assert_eq!(run.exit_code, 0, "{}", run.stderr);
    let config = config(&machine);
    assert_eq!(value(&config, "client_edge_addr"), "0.0.0.0:4000");
    assert!(!config.contains("socks_proxy"), "{config}");
    assert!(!config.contains("rpc_via_socks_proxy"), "{config}");
    assert!(run
        .stdout
        .contains("The certificate and the reverse proxy that answer at toon.example.com are yours to provide"));
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(machine.agent_node_home().join("state.json")).unwrap())
            .unwrap();
    assert_eq!(state["toon_apps"][0]["reach"]["mode"], "clearnet");
    assert_eq!(
        state["toon_apps"][0]["reach"]["hostname"],
        "toon.example.com"
    );
    assert!(!machine.agent_node_home().join("overlay").exists());
}

#[test]
fn clearnet_takes_a_hostname() {
    let machine = Machine::new();

    let run = machine.init_with(&["--clearnet", "https://toon.example.com/"]);

    assert_eq!(run.exit_code, 2, "{}", run.stdout);
    assert_eq!(run.json()["error"]["code"], "usage");
    leaves_nothing(&machine);
}
