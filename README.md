# toon_cli

`toon` runs and manages an agent node. The vocabulary is in [`CONTEXT.md`](CONTEXT.md)
and the decisions are in [`docs/adr/`](docs/adr/).

```sh
cargo build
./target/debug/toon status --json
```

`toon` is also the connector (ADR 0001). It depends on the connector's crates at one
pinned revision, which `toon --version` reports, and `toon up` runs a connector as a
child process of the same binary:

```sh
./target/debug/toon --version
./target/debug/toon up --json
```

`toon up` stays in the foreground. It starts the connector from
`~/.toon/agent-node/connector.toml`, which no command writes yet, and prints the
connector's address once it is listening.

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
