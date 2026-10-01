/mattpocock-skills:implement {{ISSUE_URL}}

You are running AFK in a sandbox, on branch `{{BRANCH}}`, which is already checked out.
Nobody will answer a question, so do not ask one. Treat the issue, its comments and its
parent spec (if it has one) as settled. Read them with `gh issue view {{ISSUE_NUMBER}} --comments`.

Commit to `{{BRANCH}}`, and reference `#{{ISSUE_NUMBER}}` in each commit message. Do not
push, open a PR or close the issue. The runner does all three once you finish.

## This repository

- This repository (`toon_cli`) is new, and it has no code and no CI yet. `CLAUDE.md` says
  how the factory works. Read `CONTEXT.md` (the vocabulary) and `docs/adr/` (decisions)
  when they exist and the ticket touches them. Where an ADR and another document disagree,
  the ADR wins. Use the glossary's terms.
- The issues labelled `wayfinder:*` are planning tickets for a human. Never work on one,
  and never edit a wayfinder map.
- After you finish, the runner runs the gate itself and won't open a PR while it is red.
  The gate is the `gate` (or `checks`) job of `.github/workflows/ci.yml` on `main`, and if
  there is none it runs nothing. Run those commands yourself before you commit if the
  file exists. Never weaken, skip or delete a test, and never loosen a lint, to get green.
- A ticket that needs a live box, a funded key or an on-chain write is not something you
  can do from here. Say so in a comment on the issue rather than guessing.

## When you cannot finish

Stop only when a genuinely new decision is needed, the action is irreversible, it touches
real funds, or it needs a credential that no workflow exposes. In that case, commit nothing
and explain what blocks you in a comment on the issue (`gh issue comment {{ISSUE_NUMBER}}`).
The runner moves an issue with no commits to `needs-triage`.

If your context is getting full (around 150k tokens) before you are done, commit what works,
write the remaining steps to `.sandcastle/logs/handoff-{{ISSUE_NUMBER}}.md`, commit it with
`git add -f`, and end your turn. A fresh session continues from your commits.

When the ticket is done and committed, output <promise>COMPLETE</promise>.

If you stopped because you're blocked, output <promise>BLOCKED</promise> instead, after your
comment on the issue. The runner then ends the run. Otherwise it starts another session, which
hits the same blocker and posts the same comment again.
