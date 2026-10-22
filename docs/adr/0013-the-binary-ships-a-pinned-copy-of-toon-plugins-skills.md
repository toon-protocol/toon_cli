# The binary ships a pinned copy of toon-plugins' skills

The skills that teach an agent to use `toon` are written in `toon-protocol/toon-plugins`, next to the data skills that name no client and the other clients' sending skills. `toon skill install` still has to work on a machine without Node or a network, and what it installs has to describe the binary that installs it. So this repo holds a copy of the `toon-nostr` and `toon-cli` plugins at one `toon-plugins` commit, named by its full SHA as ADR 0001 names the connector's, and the build compiles that copy in. A refresh is a pull request like any other.

We chose a copy in the tree over a git submodule, which every checkout and the factory sandbox would have to remember to fetch, and over fetching in `build.rs`, which would make every build, the gate included, depend on the network.

## Consequences

- A change to a skill is made in `toon-plugins` and reaches the binary only through a refresh here.
- The gate checks the pinned copy: a change here that breaks a command a shipped skill names fails here. Drift the other way, a skill on `toon-plugins` `main` naming a command `toon` doesn't have, is caught by `toon-plugins`' own CI (toon-protocol/toon-plugins#2), so another repo's commit never turns this one red.
- The `toon-cli` sending skill states the `toon_cli` versions it supports, and a test fails when this crate's version leaves that range, so a release can't ship skills that disown it.
- The `toon-client` plugin is never shipped: it describes another client.
