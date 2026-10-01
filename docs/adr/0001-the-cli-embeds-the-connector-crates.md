# The CLI embeds the connector crates

The CLI is a Rust binary that depends on the connector's crates at a pinned git revision, so the binary itself is the connector. Every other repo in the fleet runs the published image `ghcr.io/toon-protocol/connector` and pins a release handle in a compose bundle (connector ADR 0068); we chose embedding because the connector was built library-first for exactly this (connector ADR 0001), it removes Docker and the `linux/amd64` limit from the connector's side of an agent node, and it lets one binary both run a connector and manage it.

## Consequences

- The connector crates are unpublished and at `0.1.0` with no release process, so the pin is a git revision that has to be bumped by hand. A connector release does not reach an agent node until a CLI release carries it.
- The connector's config is still read once at start. Embedding does not make a connector reconfigurable in place.
- Apps are not embedded. The relay ships as an image and not as a library, and its Rust rewrite keeps it that way (relay #185), so it runs as a container.

## What the spike found (#5)

The decision holds. `toon` depends on `connector-cli` at one revision and calls its `run`, which is everything the connector's own binary does, so nothing in the connector had to change. It builds and its tests pass on x86_64, in the factory's sandbox image too. Cross-built for aarch64 and run under emulation, every test passes except the one that reads the connector's executable out of `/proc`, which sees the emulator there; CI runs the tests on an arm64 runner.

- Cargo ignores a git dependency's lockfile. Resolved fresh, 113 of the connector's 600 packages came out at other versions than the connector is tested with, including the five it holds back on purpose. `Cargo.lock` here is therefore seeded from the connector's at the pinned revision, and a bump of the pin re-seeds it.
- The connector's fake chain is two things, and only one is reachable. `FakeRpc` is behind a feature of `connector-chain-rpc` and works as a test dependency: answering as a chain with x402 deployed, it carries a connector through start. The in-memory settlement is not reachable from a connector started from a config file, because `connector_cli::build` constructs the chain-backed backends itself and its `Runtime` holds their concrete types. A command-line test that opens, funds or lands a channel needs either a seam in the connector to hand it a settlement backend, or a local chain such as `anvil`.
- The binary carries both chains' SDKs whether or not a connector settles on them: about 660 crates in the lockfile, where the skeleton had 40, and a release binary of 27 MB.
