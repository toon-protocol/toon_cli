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
against a temporary home directory, and a test asserts on its output, its exit code and
the files it leaves. Exit codes and error codes are part of the interface and are listed
in `docs/exit-codes.md`; `tests/exit_codes.rs` fails when a code is missing from it.

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
check in the `gate` job a plain `run:` step. The shared sandbox image builds and tests
the crate with the same `stable` toolchain CI installs.

The runner's own commands, from `.sandcastle/`: `npm ci`, `npm test`, `npm run typecheck`.
The repository root has no `package.json`; the runner's only Node manifest is
`.sandcastle/package.json`. The sandbox image is the shared
`ghcr.io/toon-protocol/sandcastle-agent`; this repo has no Dockerfile for it.
`close-linked-issues.yml` is identical in every factory repo, so change it in
`toon-protocol/provider` first.
