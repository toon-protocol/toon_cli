# toon_cli

`toon` runs and manages an agent node. The vocabulary is in [`CONTEXT.md`](CONTEXT.md)
and the decisions are in [`docs/adr/`](docs/adr/). Protocol that relays and agents agree
on and no existing NIP covers is drafted in [`nips/`](nips/).

## Install

From a release, on Linux x86_64 or aarch64 with glibc 2.35 or later, no Rust toolchain
needed:

```sh
version=v0.1.0   # a tag from https://github.com/toon-protocol/toon_cli/releases
arch=$(uname -m) # x86_64 or aarch64
curl -fsSLO "https://github.com/toon-protocol/toon_cli/releases/download/$version/toon-$version-linux-$arch.tar.gz"
curl -fsSLO "https://github.com/toon-protocol/toon_cli/releases/download/$version/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "toon-$version-linux-$arch.tar.gz"
install -D "toon-$version-linux-$arch/toon" ~/.local/bin/toon
```

From source, with a Rust toolchain:

```sh
cargo install --locked --git https://github.com/toon-protocol/toon_cli --tag v0.1.0
```

Pushing a tag `v<version>` (the version in `Cargo.toml`) publishes a release
(`.github/workflows/release.yml`).

## Build

```sh
cargo build
./target/debug/toon status --json
```

`toon` is also the connector (ADR 0001). It depends on the connector's crates at one
pinned revision, which `toon --version` reports together with the pinned relay image
(`relay_image` in `Cargo.toml`), and `toon up` runs a connector as a
child process of the same binary:

```sh
./target/debug/toon --version
./target/debug/toon up --json
```

`toon up` writes a `systemd --user` unit (`~/.config/systemd/user/toon-agent-node.service`)
that runs `toon up --foreground`, the supervisor, and starts it, so the agent node
outlives the session that started it. The supervisor restarts a connector that exits,
`toon status` shows how often, `toon logs <name>` shows the log of a TOON app or an app,
and `toon down` stops the supervisor and the unit. `toon up --foreground` runs the
supervisor in this process instead, and prints the connector's address once it is
listening.

Every command is non-interactive and accepts `--json`, which prints exactly one JSON
document. Exit codes and error codes are stable and listed in
[`docs/exit-codes.md`](docs/exit-codes.md).

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

These are the steps of the `gate` job in `.github/workflows/ci.yml`.
