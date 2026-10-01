# toon_cli

`toon` runs and manages an agent node. The vocabulary is in [`CONTEXT.md`](CONTEXT.md)
and the decisions are in [`docs/adr/`](docs/adr/).

```sh
cargo build
./target/debug/toon status --json
```

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
