## Agent skills

### Issue tracker

Issues live in GitHub Issues on `toon-protocol/toon_cli`, managed with the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default triage labels are used as-is: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `CONTEXT.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.

## The crate

One Rust crate, `toon-cli`, at the repository root; it builds the binary `toon`.
Tests live in `tests/` and go through one seam: `tests/support` runs the built binary
against a temporary home directory, and a test asserts on its output, its exit code,
the files it leaves, and what a connector it started answers over loopback. Exit codes and error codes are part of the interface and are listed
in `docs/exit-codes.md`. `tests/exit_codes.rs` keeps its own copy of both lists and
checks it against that file and `toon --help`, so add a new code to all of them together.

## Draft NIPs

`nips/` holds the draft NIPs that ship with the CLI, each written from `nips/TEMPLATE.md`
and listed in `nips/README.md`; `tests/nips.rs` checks both by reading the files, as
`tests/exit_codes.rs` reads its document. A draft is the single source
for what implements it: `nips/paid-subscription.md` for the subscribe commands, the fake
remote relay and the Rust relay, `nips/proposals-as-events.md` for `toon nip publish` and
the skill for authoring a NIP. Change the draft before the code that follows it.

## The embedded connector

`toon` depends on the connector's crates at one git revision (ADR 0001). The `rev` is
written on every connector line in `Cargo.toml`; `build.rs` fails the build if they
differ and hands the value to `toon --version`. Do not copy it anywhere else.

To move the pin: change every `rev` in `Cargo.toml`, replace `Cargo.lock` with the
connector's own `Cargo.lock` at that revision, then run `cargo build` once without
`--locked`. Cargo does not read a git dependency's lockfile, so this is what keeps the
embedded connector on the dependency versions the connector was tested with.

`toon up` starts `toon connector <config>`, a hidden command, as a child process
(`src/connector.rs`). Tests that start a connector use `tests/support/fake_chain.rs`,
the connector's `FakeRpc` answering as an EVM chain with x402 deployed. It holds no
channels and accepts no transaction: enough for a connector to start, not for a test
that moves money.

## The AFK factory

`ready-for-agent` is the factory's queue (`docs/agents/triage-labels.md`). When an issue
carries it and its blockers are closed, `.github/workflows/agent-implement.yml` runs
`.sandcastle/agent-implement-issue.ts`, which runs `/mattpocock-skills:implement`
(Sonnet 5.5), then `/mattpocock-skills:code-review` against `main` in a fresh session
(Opus 5.5), then the gate, then pushes `sandcastle/issue-N` and opens a PR labelled
`ready-for-human`. A failed run moves the issue to `needs-triage`. Specs, blocked issues,
issues with an open PR and any issue labelled `wayfinder:*` are skipped
(`.sandcastle/ready-issues.ts`). Nothing in the factory merges a PR: a human does.

**The gate reads its steps from CI.** The gate is the `run:` steps of the `gate` job
(else the `checks` job) of `.github/workflows/ci.yml` **on `main`**, in order. If there is
no `ci.yml`, no such job, or no runnable step, the gate runs nothing and logs that loudly.
The rule lives in one place, `gateFromCi` in `.sandcastle/run-gate.ts`, tested by
`.sandcastle/run-gate.test.ts`.

The gate today, run from the repository root:

- `cargo fmt --all -- --check`
- `cargo clippy --all-targets --locked -- -D warnings`
- `cargo test --locked`

A step with an `if:` or a `${{ }}` expression is skipped by the factory, so keep every
check in the `gate` job a plain `run:` step. CI and the shared sandbox image both install
Rust `stable` with clippy and rustfmt, and neither pins a version, so the two can differ
by a release until the image is rebuilt.

The runner's own commands, from `.sandcastle/`: `npm ci`, `npm test`, `npm run typecheck`.
The repository root has no `package.json`; the runner's only Node manifest is
`.sandcastle/package.json`. The sandbox image is the shared
`ghcr.io/toon-protocol/sandcastle-agent`; this repo has no Dockerfile for it.
`close-linked-issues.yml` is identical in every factory repo, so change it in
`toon-protocol/provider` first.
