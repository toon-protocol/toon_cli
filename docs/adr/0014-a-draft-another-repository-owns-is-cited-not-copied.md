# A draft another repository owns is cited, not copied

A draft in `nips/` is the single source for everything that implements it, and it ships with the CLI. Some drafts the CLI implements are owned elsewhere: the tip draft (`toon tip`) and the Deployment draft (`toon deployment`, kind 30437) are written in `toon-protocol/relay`, because the relay's UI reads those events and relay's ADRs decide their shape. So `nips/` holds only the drafts this repository owns. A draft another repository owns is named in `nips/README.md` with a link to its file on that repository's `main`, and the code and tests that follow it cite that file. Nothing is copied.

We chose a citation over a copy in `nips/`, which would give the draft two sources that drift apart with no test to notice, and over vendoring at a pinned commit as ADR 0013 does for skills, because a draft is read by the people implementing it, not compiled into the binary.

## Consequences

- A change to a cited draft is made in its own repository first, and the CLI follows in an ordinary PR, as for a draft here.
- Work on a command that follows a cited draft waits until the draft is on its owner's `main`, so the link resolves.
- `tests/nips.rs` checks only the drafts in `nips/`. Nothing here notices when a cited draft changes under the CLI.
