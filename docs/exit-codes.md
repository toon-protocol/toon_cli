# Exit codes and error codes

Both are part of `toon`'s interface. A code is added, and never renumbered or given a
new meaning, so a script or an agent can branch on it across releases.
`tests/exit_codes.rs` fails if this file and the binary disagree.

## Output

- With `--json`, a command prints exactly one JSON document on standard output and
  nothing on standard error, whether it succeeded or failed. `--help` is the one
  exception: it is always text.
- Without `--json`, a command prints readable text on standard output, and an error
  goes to standard error as `error: <message>`.
- No command reads standard input or prompts.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The command did what was asked |
| 1 | The command failed; the error's code says why |
| 2 | The command line was not understood |
| 3 | There is no agent node on this machine |

`toon status` exits with the code that describes the agent node, and still prints its
report: on a machine with no agent node it prints `{"home": "<path>", "agent_node": null}`
and exits 3.

## Errors

A failed command with `--json` prints:

```json
{ "error": { "code": "usage", "message": "unrecognized subcommand 'frobnicate'" } }
```

`code` is stable. `message` is for a person to read and may be reworded.

| Error code | Exit code | Meaning |
| --- | --- | --- |
| `usage` | 2 | The command line was not understood: an unknown command or flag, or a missing argument |
| `home_unresolved` | 1 | `HOME` is not set, so there is nowhere to look for an agent node |
