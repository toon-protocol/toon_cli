mod support;

use std::fs;

use support::fake_chain::FakeChain;
use support::Machine;

/// The relay image this build pins, as `toon --version --json` reports it.
fn relay_image(machine: &Machine) -> String {
    machine.toon(&["--version", "--json"]).json()["relay_image"]
        .as_str()
        .expect("a relay image")
        .to_owned()
}

/// Every form of the relay's image: pinned, another tag, no tag, a digest alone.
fn relay_images(machine: &Machine) -> Vec<String> {
    let pinned = relay_image(machine);
    let (name, digest) = pinned.split_once('@').expect("pinned by digest");
    let repository = name
        .rsplit_once(':')
        .filter(|(_, tag)| !tag.contains('/'))
        .map_or(name, |(r, _)| r);
    vec![
        pinned.clone(),
        format!("{repository}:other"),
        repository.to_owned(),
        format!("{repository}@{digest}"),
    ]
}

fn entries(machine: &Machine) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(machine.agent_node_home())
        .unwrap()
        .flatten()
        .flat_map(|entry| {
            let path = entry.path();
            let top = entry.file_name().to_string_lossy().into_owned();
            let inner: Vec<String> = fs::read_dir(&path)
                .map(|dir| {
                    dir.flatten()
                        .map(|e| format!("{top}/{}", e.file_name().to_string_lossy()))
                        .collect()
                })
                .unwrap_or_default();
            std::iter::once(top).chain(inner)
        })
        .collect();
    names.sort();
    names
}

#[test]
fn create_and_add_refuse_the_relays_image_and_change_nothing() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);
    let status = machine.toon(&["status", "--json"]).json();
    let files = entries(&machine);

    for image in relay_images(&machine) {
        for args in [
            vec!["create", "second", "--image", &image, "--no-peer", "--json"],
            vec![
                "create",
                "second",
                "--image",
                &image,
                "--deposit",
                "5",
                "--clearnet",
                "second.example.com",
                "--json",
            ],
            vec![
                "add", "second", "--to", "relay", "--image", &image, "--json",
            ],
        ] {
            let run = machine.toon(&args);
            assert_eq!(run.exit_code, 1, "{args:?}: {}", run.stdout);
            assert_eq!(run.json()["error"]["code"], "one_relay", "{args:?}");
            assert_eq!(machine.toon(&["status", "--json"]).json(), status);
            assert_eq!(entries(&machine), files);
        }
    }
}

#[test]
fn an_image_from_another_repository_is_not_refused() {
    let chain = FakeChain::start();
    let machine = Machine::new();
    assert_eq!(machine.init_on(&chain).exit_code, 0);

    let run = machine.toon(&[
        "add", "second", "--to", "relay", "--image", "notes:1", "--json",
    ]);

    assert_ne!(run.json()["error"]["code"], "one_relay");
}
