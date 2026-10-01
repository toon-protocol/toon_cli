# Exit codes and error codes

Both are part of `toon`'s interface. A code is added, and never renumbered or given a
new meaning, so a script or an agent can branch on it across releases.
`tests/exit_codes.rs` holds a copy of both lists and fails when the exit codes here,
the ones `toon --help` prints, and that copy differ, or when an error code in that copy
is missing here. Add a new code to the test and to this file together.

## Output

- With `--json`, a command prints exactly one JSON document on standard output and
  nothing on standard error, whether it succeeded or failed. `--help` is the one
  exception: it is always text.
- Without `--json`, a command prints readable text on standard output, and an error
  goes to standard error as `error: <message>`.
- No command reads standard input or prompts.
- A command that stays in the foreground, such as `toon up`, prints its one JSON
  document once what it runs is up, and then keeps running. If it stops later, its exit
  code says so and it prints no second document.
- `toon connector` is not an operator's command and keeps none of these rules. It is
  hidden, `toon up` starts it as a child process, and it reports to its supervisor.
- `help` is not a command. Ask for help with `--help`.
- If the output cannot be written, the command exits 1 whatever it found.

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
| `home_unresolved` | 1 | `HOME` is not set or is empty, so there is nowhere to look for an agent node |
| `no_wallet` | 3 | There is no wallet on this machine: run `toon init` |
| `passphrase_missing` | 1 | Neither `TOON_PASSPHRASE_FILE` nor `TOON_PASSPHRASE` is set, or the passphrase is empty |
| `passphrase_unreadable` | 1 | `TOON_PASSPHRASE_FILE` names a file that cannot be read, or the passphrase is not valid UTF-8 |
| `passphrase_wrong` | 1 | The passphrase does not open the keystore |
| `keystore_corrupt` | 1 | The keystore file is not one this version reads |
| `io` | 1 | A file or the system's randomness could not be used |
| `no_agent_node` | 3 | The command needs an agent node and this machine has none |
| `connector_failed` | 1 | A connector did not start, or stopped; the message carries the connector's own reason |

## The wallet passphrase

`toon init` and `toon wallet show` read the passphrase from the file named by
`TOON_PASSPHRASE_FILE`, else from `TOON_PASSPHRASE`. It is never a flag. One trailing
newline in the file is not part of the passphrase.
