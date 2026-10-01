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
- A command that stays in the foreground, such as `toon up --foreground`, prints its one JSON
  document once what it runs is up, and then keeps running. It stops when `toon down`
  asks it to, exits 0 and prints no second document. If an app behind its connector
  stops, it stops too and exits 1, also with no second document.
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

`toon up` without `--foreground` installs a `systemd --user` unit and returns once
`systemctl` has started it. A connector that stops by itself is restarted by the
supervisor, and `toon status` reports how many times.

`toon status` exits with the code that describes the agent node, and still prints its
report. It exits 1 when the supervisor or a connector is not running, and `toon down` exits 0
whether or not anything was running, unless `systemctl` would not stop the unit. `toon send` exits 1 when the packet was rejected, and still prints its report: the
reject code is in `reject.code`. It exits 1 too, with `"outcome": "wrong_fulfilment"`, when
the packet was fulfilled with a fulfilment that does not match it. On a machine with no agent node, `toon status` prints `{"home": "<path>", "agent_node": null}`
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
| `connector_failed` | 1 | A connector did not start, or the supervisor did not stop when `toon down` asked; the message carries the connector's own reason |
| `already_running` | 1 | A supervisor is already running this agent node: `toon down` stops it |
| `app_failed` | 1 | An app behind a connector did not start, or stopped; the message carries the reason |
| `unfunded` | 1 | A settlement key does not hold what the connector needs, so `toon up` did not start it; the message names each address and the amount |
| `faucet_unavailable` | 1 | `toon wallet fund` has no faucet to ask: the network is not the devnet, or the faucet did not answer or refused |
| `not_running` | 1 | The command needs the agent node's connector running: run `toon up` |
| `send_failed` | 1 | The packet (or, for `toon event publish` and `toon nip publish`, the event) could not be sent: the connector's operator surface refused the write or could not be reached; the message carries the reason |
| `systemd_failed` | 1 | `toon up` wrote its `systemd --user` unit and `systemctl` would not load or start it, or `toon down` could not stop it; the message carries `systemctl`'s own reason |
| `unknown_name` | 1 | `--app`, `toon create --app`, `toon destroy`, `toon logs`, `toon add` or `toon remove` was given a name that is not a TOON app or an app of this agent node, as that command needs |
| `peer_failed` | 1 | The connector's operator surface refused a peering write or could not be reached; the message carries the reason |
| `peer_not_peerable` | 1 | `peer add` named a connector that is not peerable: the refusal is on the other side, and only its operator can lift it |
| `route_failed` | 1 | The connector's operator surface refused a route write or could not be reached, or a route command was given an address prefix it cannot use or that no route has; the message carries the reason |
| `chain_failed` | 1 | A chain's JSON-RPC endpoint could not be reached or did not answer a read as expected; the message carries the reason |
| `channel_failed` | 1 | A channel write was refused by the connector or could not be sent, the channel id is not one, or the terms file was unreadable; the message carries the reason |
| `name_taken` | 1 | `toon add` or `toon create` was given a name that is not usable, or that a TOON app or an app of this agent node already has |
| `query_failed` | 1 | `toon event query`, or `toon nip publish` asking for a draft's current revision, could not read events from the relay: it did not answer, is not a websocket relay this build dials, or closed the subscription with a reason the message carries |
| `draft_refused` | 1 | `toon nip new` or `toon nip publish` would not write or publish a draft: the file exists already, does not name a draft or begin with its title, is not UTF-8, or the relay holds the identifier under another title and `--title-changed` was not given; the message says which |
| `confirmation_required` | 1 | `toon add`, `toon remove`, `toon route price`, `toon relay config` or `toon relay price` restarts a running connector, which drops the packets it holds in flight (`toon relay` restarts the relay too), and was not given `--yes`; nothing was changed |
| `overlay_unavailable` | 1 | The Anyone overlay did not bootstrap, so a hidden service was not created or started; nothing falls back to clearnet |
| `not_confirmed` | 1 | A command that moves money was run without `--yes`, so it did nothing |
| `spending_limit` | 1 | A payment is over the per-command limit or what is left of the day's, or the spending limit is missing or was not signed by the wallet; the message says which limit and how much remains |
| `funds_held` | 1 | `toon destroy` did nothing: a channel of the TOON app still holds funds, or its channels could not be read; the message names each |
| `last_toon_app` | 1 | `toon destroy` was given the only TOON app: an agent node always has one |


## The wallet passphrase

`toon init`, `toon wallet show`, `toon wallet balances`, `toon wallet backup`, `toon wallet restore` `toon event publish` and `toon nip publish` read the passphrase from the file named by
`TOON_PASSPHRASE_FILE`, else from `TOON_PASSPHRASE`. It is never a flag. One trailing
newline in the file is not part of the passphrase.

`toon wallet fund` reads the settlement keys the agent node holds, not the keystore, so it
needs no passphrase.

## Network profiles and funding

`toon init --network` takes `devnet` (the default), `sandbox` or `mainnet`. Solana
settlement is off unless `--solana` is given. `toon up` does not start a connector whose
settlement key holds less than 0.0001 ETH (0.01 SOL) for gas and one whole token; it fails
with `unfunded`. Only the devnet has a faucet: on the other networks `toon wallet fund`
fails with `faucet_unavailable`, and on mainnet the operator funds the addresses themselves.

## Hidden service and clearnet

A new TOON app is a hidden service (ADR 0003): its connector is reachable only at its onion
endpoint, a `.anyone` address, on port 80, and the relay's read port at the same address on
port 7100. Its connector listens on loopback only, and all of its outbound traffic goes
through the overlay's SOCKS proxy, settlement RPC included. The endpoint is made from a key
the wallet derives, so it is the same after a restart, and a wallet backup restores it.

`toon init` creates a hidden service only with `--accept-anyone-terms`, which says the
operator agrees to the Anyone Protocol's terms; without it `init` fails with `usage` and
creates nothing. If the overlay cannot bootstrap, `init` fails with `overlay_unavailable`,
creates nothing and leaves nothing listening: it never falls back to clearnet.

`toon init --clearnet <hostname>` asks for clearnet instead. The connector binds the
`--listen` address (`127.0.0.1:0` unless given), and the certificate and the reverse proxy
that answer at the hostname are the operator's to provide. A clearnet TOON app needs no
overlay and no terms flag.

A hidden service hides where the TOON app is reachable and not who it pays: payments are
on a public chain. `toon init` says so.

## Events

`toon event publish` signs with the agent identity, which is why it needs the passphrase,
and sends the event to the agent node's own relay as an operator write: it exits 1 with
`"outcome": "rejected"` when the packet is rejected, `"outcome": "refused"` when the relay
answers with a status that is not 2xx, and `"outcome": "wrong_fulfilment"` when the packet is
fulfilled but not by this connector. `toon event query` is a plain NIP-01 `REQ` and needs no
passphrase; it reads from `ws://` relays only.

`toon nip publish` signs a draft (`nips/proposals-as-events.md`) the same way and writes it
to the agent node's own relay as `toon event publish` does, with the same outcomes. It first
asks `--relay` for the draft's current revision, so `--relay` must be the agent node's own
relay's `ws://` URL. It exits 1 with `draft_refused` when the file is not a draft it can
publish, or the relay holds that identifier under another title and `--title-changed` was
not given.

## Backup and restore

`toon wallet backup --out <file>` seals the keystore's mnemonic and the address key of every
onion endpoint into one file under the wallet's passphrase, and refuses a file that exists.
`toon wallet restore <file>` recreates the wallet and the address keys in a home with no
wallet or agent node (`io` if there is one), sealed under the passphrase that opened the backup; a wrong
passphrase fails with `passphrase_wrong` and a file that is not a backup with
`keystore_corrupt`. Then `toon init` makes the TOON app on the keys it finds, at the same
onion endpoints.

`toon init --from-mnemonic` restores a wallet from a mnemonic alone, read from the file named
by `TOON_MNEMONIC_FILE`, else from `TOON_MNEMONIC`, never a flag, into a home with no wallet
(`io` if there is one; `usage` if the mnemonic is missing or not BIP-39). It shows no mnemonic, and
a hidden service gets new onion endpoints, which it says (`"onion_endpoints_changed": true`):
a mnemonic does not hold the address keys.

## Creating and destroying a TOON app

`toon create <name> --app <from>` makes a second TOON app (ADR 0002): a connector on the next
unused index of the wallet's keys, and an app behind it, named `<name>` too and reached at
`g.toon.<name>`. `--image` or `--url` says which app; `--clearnet`, `--accept-anyone-terms` and
`--listen` say how the connector is reached, as for `toon init`. It reads the wallet passphrase.
It is peered with `<from>`, the first TOON app if `--app` is left out, in both directions,
each channel opened with `--deposit`, with a forwarding route each way; `--no-peer` says not to.
The deposits move money, so they need `--yes` and count twice against the spending limit. A
settlement key that holds too little fails with `unfunded`, and nothing is created: the message
names the address to fund. A running supervisor starts the connector; otherwise `toon up` does.

`--app <name>` on any command that talks to a connector, such as `toon send`, `toon peer`,
`toon route` and `toon channel`, says which TOON app it is about; the first TOON app is the
default.

`toon destroy <name>` stops a TOON app's connector and removes its files and its app's data,
except an onion key. It needs the TOON app running, to read its channels, and fails with
`funds_held`, naming each channel, while an outbound channel holds collateral or an inbound one
a voucher that has not landed. It never removes the last TOON app (`last_toon_app`).

## The spending limit

Every command that moves money (`toon send`, `toon peer add`) states its amount and needs
`--yes`. The amount is checked against the spending limit before the command runs: at most
`--max-per-command` for one command, and `--max-per-day` for the commands of one UTC day
together, both in the token's base units, set at `toon init` (defaults 10000000 and
100000000). A payment that was rejected is not counted, nor one that failed before it
reached the connector or that the other side refused; any other failure may have paid, and
stays counted. `toon limit show` prints the limits and what is left today. `toon limit set`
changes them and reads the wallet passphrase, so an agent without it cannot raise them: the
limits are signed with a key the wallet derives, and an unsigned or edited `limits.json`
stops every payment. When `limits.json` is missing or was edited, `toon limit set` needs
both `--max-per-command` and `--max-per-day`.
