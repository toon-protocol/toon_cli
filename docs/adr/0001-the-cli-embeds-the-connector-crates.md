# The CLI embeds the connector crates

The CLI is a Rust binary that depends on the connector's crates at a pinned git revision, so the binary itself is the connector. Every other repo in the fleet runs the published image `ghcr.io/toon-protocol/connector` and pins a release handle in a compose bundle (connector ADR 0068); we chose embedding because the connector was built library-first for exactly this (connector ADR 0001), it removes Docker and the `linux/amd64` limit from the connector's side of an agent node, and it lets one binary both run a connector and manage it.

## Consequences

- The connector crates are unpublished and at `0.1.0` with no release process, so the pin is a git revision that has to be bumped by hand. A connector release does not reach an agent node until a CLI release carries it.
- The connector's config is still read once at start. Embedding does not make a connector reconfigurable in place.
- Apps are not embedded. The relay ships as an image and not as a library, and its Rust rewrite keeps it that way (relay #185), so it runs as a container.
