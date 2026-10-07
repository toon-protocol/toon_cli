# Running it day to day

## Start, stop, inspect

```sh
toon up                  # install and start the systemd --user unit, then return
toon up --foreground     # run the supervisor in this terminal instead
toon status              # supervisor, each TOON app and app, restarts
toon logs relay          # the log of a TOON app or of an app
toon logs echo -n 200    # the last 200 lines
toon down                # stop everything, and the unit
```

`toon up` writes `~/.config/systemd/user/toon-agent-node.service`, which runs
`toon up --foreground`, and starts it. The supervisor:

- starts the relay's container and every app, then every connector, each a child process of
  the same binary;
- restarts a connector that exits, and `toon status` counts how often, with the last reason;
- stops, and exits 1, if the relay stops;
- keeps every paid subscription's feed flowing into the relay.

To keep it running after you log out, let your user's services outlive the session:

```sh
loginctl enable-linger "$USER"
```

## Counting, listing and watching packets

```sh
toon packet count          # fulfilled, rejected, rejects by code, fees earned
toon packet count --json
toon --app second packet count
```

`toon packet count` is free: it needs no passphrase and moves no money. It reads the connector's
metrics and prints the packets it fulfilled and rejected, the rejects by RFC-0027 code such as
`F02`, and the fees earned in the token's base units. A connector that has carried nothing prints
zeros. The counts are since the connector last started: `toon` keeps no totals, so a restart of
the connector, which `toon add` and `toon remove` cause, begins again at 0. It fails as
`toon peer list` does when the agent node is not running.

```sh
toon packet list           # the last 20 rejected packets, newest first
toon packet list -n 5 --json
```

`toon packet list` is free too: it needs no passphrase and moves no money. It reads the connector's
log, as `toon logs` does, so it works while the agent node is stopped, and the log keeps the
rejects from before a restart. A line shows the time, the destination, the reject code and the
reject message. Only rejected packets are listed: the connector logs a fulfilled packet only at
`debug`, so `toon packet count` has those, as a count. `-n` / `--limit` says how many to show. A
connector that has rejected nothing prints an empty list and exits 0.

```sh
toon packet history        # the last 20 packets the connector handled, newest first
toon packet history -n 5 --json
```

`toon packet history` is free too: it needs no passphrase and moves no money. It reads the
connector's packet history, which `toon` turns on at 1,000 packets in every connector config it
writes, so an agent node set up earlier has it after its next `toon up`. Unlike `toon packet list`,
which reads rejects from the log, it lists fulfilled and rejected packets alike, and a line shows
the time, the packet's **direction**, the destination, the peering or channel it came `from` and
the peering it went `to`, the amount, the fee of a fulfilled forward, the outcome and, for a
reject, its code and message. The direction is `delivered` when the packet ended at one of the
connector's apps, `forwarded` when it arrived from one side and left toward a peer, and `sent`
when you originated it. A packet that expired or could not be routed has no direction, and a field
a row lacks is left out. The history is recent packets only and is forgotten when the connector
restarts; `toon packet count` has the totals. When the connector had to drop rows, the text says
how many (`dropped` in `--json`). It needs the agent node running and fails with `not_running`
otherwise. `-n` / `--limit` says how many to show; a connector that has handled nothing prints an
empty list and exits 0.

## Scripting with `--json`

Every command takes `--json` and then prints **exactly one JSON document** on standard
output and nothing on standard error, whether it succeeded or not. No command prompts or
reads standard input. That makes every command safe to drive from a script or an agent:

```sh
if ! out=$(toon send "$ADDR" --amount 10 --yes --json); then
  case $(jq -r '.error.code // .outcome' <<<"$out") in
    spending_limit) echo "over the limit"; exit 1 ;;
    not_running)    toon up ;;
    rejected)       jq -r '.reject.code' <<<"$out" ;;
  esac
fi
```

Branch on `error.code` and the exit code, never on `message`, which may be reworded.

| Exit code | Meaning |
| --- | --- |
| 0 | Done |
| 1 | Failed; `error.code` says why |
| 2 | The command line was not understood |
| 3 | There is no agent node on this machine |

Every error code, with its meaning, is in [`docs/exit-codes.md`](../exit-codes.md).

## Environment

| Variable | What it does |
| --- | --- |
| `TOON_PASSPHRASE_FILE` | The file holding the wallet passphrase (preferred) |
| `TOON_PASSPHRASE` | The passphrase itself, when there is no file |
| `TOON_MNEMONIC_FILE`, `TOON_MNEMONIC` | The mnemonic for `toon init --from-mnemonic` |
| `TOON_ANON_MIRROR` | Where the `anon` release is downloaded from |
| `TOON_TRUSTED_ROOT` | A PEM file of extra roots to trust for `wss://` relays |

## Where things live

Everything is under `~/.toon/agent-node`:

| Path | What it is |
| --- | --- |
| `keystore.json` | The wallet, sealed by the passphrase |
| `limits.json` | The spending limit, signed by the wallet |
| `state.json` | The TOON apps, their apps, routes and prices, the network |
| `connectors/<n>/` | Each connector's config, keys, state and log |
| `apps/<name>/` | Each app's identity key, and its `data/`, mounted at `/data` |
| `overlay/` | The `anon` daemon and its state |

Change it with `toon` commands, not by hand. An edited `limits.json` stops every payment.

## When something goes wrong

| You see | Do this |
| --- | --- |
| `no_agent_node`, exit 3 | `toon init` |
| `passphrase_missing` | Set `TOON_PASSPHRASE_FILE` |
| `not_running` | `toon up` |
| `already_running` | It is up already; `toon down` first if you meant to restart |
| `unfunded` | Fund the address the message names ([Money](money.md#funding)) |
| `not_confirmed` | The command moves money; read the amount, then add `--yes` |
| `confirmation_required` | The command restarts a connector; add `--yes` when that is fine |
| `spending_limit` | Report it, or have the passphrase holder run `toon limit set` |
| `peering_needed` | Run the `peer add` (if the message prints one) and `route add` the message prints |
| `overlay_unavailable` | The Anyone network did not carry: try again later |
| `describe_failed` | The connector at that `/ilp` URL gave no self-description: check the URL, or try again later |
| `connector_failed`, `app_failed` | `toon logs <name>` |
| `systemd_failed` | `systemctl --user status toon-agent-node` |
