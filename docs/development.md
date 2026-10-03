# Developing toon

One Rust crate at the repository root builds the binary `toon`. [`CLAUDE.md`](../CLAUDE.md)
describes how the repository is worked on, including by the AFK factory.

## Build

```sh
cargo build
./target/debug/toon --version
./target/debug/toon status --json
```

`toon` is also the connector (ADR 0001). It depends on the connector's crates at one pinned
revision, written on every connector line of `Cargo.toml`; `build.rs` fails the build if they
differ and hands the value to `toon --version`, together with the pinned relay image
(`relay_image` in `Cargo.toml`). `toon up` runs each connector as `toon connector <config>`,
a hidden command of the same binary.

## Checks

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

These are the steps of the `gate` job in `.github/workflows/ci.yml`.

The tests run the built binary against a temporary home on loopback, with stand-ins for the
overlay, the relay's container, the chain and another operator's relay.
`TOON_APP_COMMAND=<program>` runs every app as a local process of that program instead of a
container; the tests use it with `examples/fake_relay.rs`.

[`docs/end-to-end.md`](end-to-end.md) is the run that uses the real ones, by hand, against
the `infra` sandbox, before a release.

## Interface rules

Exit codes and error codes are part of the interface and listed in
[`exit-codes.md`](exit-codes.md); `tests/exit_codes.rs` keeps a copy of both and checks it
against that file and `toon --help`, so add a new code to all three together. The draft
NIPs in [`nips/`](../nips/) are the single source for what implements them: change the draft
before the code.

## Release

Pushing a tag `v<version>`, the version in `Cargo.toml`, publishes a release
(`.github/workflows/release.yml`).
