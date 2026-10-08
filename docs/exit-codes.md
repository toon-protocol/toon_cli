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
reject code is in `reject.code`, and `paid` is what the packet cost (see Spending limit). It
exits 1 too, with `"outcome": "wrong_fulfilment"`, when the packet was fulfilled with a fulfilment that does not match it. `toon probe` sends a packet as `toon send` does and exits 0 on a reject that states a cost, complete or partial (`cost` and `complete` in the report, `cost` possibly `"0"`), and on a fulfil; it exits 1 on a reject that states none (the connector's rule says which: no route, a peer that could not be reached and the like), when the answer was not understood or did not come, and on a wrong fulfilment. It has no exit code or error code of its own. On a machine with no agent node, `toon status` prints `{"home": "<path>", "agent_node": null}`
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
| `unfunded` | 1 | A settlement key does not hold what is needed, so `toon up` did not start the connector (the token; on Solana also SOL), or a command that has a connector send a transaction did not (EVM gas, 0.001 ETH, whether the check finds too little or the connector reports the chain refused to estimate for lack of it: `toon join`, `toon peer add` with a deposit, `toon create` with a deposit, `toon channel open`, `fund`, `withdraw`, `land`); the message names each address and the amount |
| `faucet_unavailable` | 1 | `toon wallet fund` has no faucet to ask: the network is not the devnet, or the faucet did not answer or refused every address (one refusal does not fail the command while another address is funded) |
| `not_running` | 1 | The command needs the agent node's connector running: run `toon up` |
| `send_failed` | 1 | The packet (or, for `toon event publish` and `toon nip publish`, the event; for `toon message send`, a wrap) could not be sent: the connector's operator surface refused the write, could not be reached, or did not answer within the wait (the packet's 30-second expiry and five seconds more); the message carries the reason. A packet that went unanswered has expired and will not be delivered, so the command can be run again; the failure's JSON carries `paid` and, for `toon event publish --relay`, the `event` with its id (see Spending limit) |
| `systemd_failed` | 1 | `toon up` wrote its `systemd --user` unit and `systemctl` would not load or start it, or `toon down` could not stop it; the message carries `systemctl`'s own reason |
| `unknown_name` | 1 | `--app`, `toon create --app`, `toon destroy`, `toon logs` (also an app served at a URL, which has no log of its own), `toon add` or `toon remove` was given a name that is not a TOON app or an app of this agent node, as that command needs |
| `peer_failed` | 1 | The connector's operator surface refused a peering write or could not be reached; the message carries the reason |
| `peer_not_peerable` | 1 | `peer add` named a connector that is not peerable: the refusal is on the other side, and only its operator can lift it |
| `route_failed` | 1 | The connector's operator surface refused a route write or could not be reached, or a route command was given an address prefix it cannot use or that no route has; the message carries the reason |
| `chain_failed` | 1 | A chain's JSON-RPC endpoint could not be reached or did not answer a read as expected; the message carries the reason |
| `channel_failed` | 1 | A channel write was refused by the connector or could not be sent, the channel id is not one, or the terms file was unreadable; the message carries the reason |
| `name_taken` | 1 | `toon add` or `toon create` was given a name that is not usable, or that a TOON app or an app of this agent node already has |
| `query_failed` | 1 | `toon event query`, or `toon nip publish` asking for a draft's current revision, could not read events from the relay: it did not answer, is not a `ws://` or `wss://` relay (or its certificate did not verify), or closed the subscription with a reason the message carries; or `toon relay subscriptions --incoming` could not read the running relay's list of subscribers, or the agent node's relay does not sell its live feed |
| `draft_refused` | 1 | `toon nip new` or `toon nip publish` would not write or publish a draft: the file exists already, does not name a draft or begin with its title, is not UTF-8, or the relay holds the identifier under another title and `--title-changed` was not given; the message says which |
| `confirmation_required` | 1 | `toon add`, `toon remove`, `toon route price`, `toon relay config` or `toon relay price` restarts a running connector, which drops the packets it holds in flight (`toon relay` restarts the relay too), and was not given `--yes`; nothing was changed |
| `overlay_unavailable` | 1 | The Anyone overlay did not bootstrap (its `anon` release could not be downloaded or did not match its pinned checksum, its terms were not agreed to, or the daemon did not come up), so a hidden service was not created or started, or a command on a hidden agent node that makes a request of its own (a faucet, a chain, a relay, a connector) had no overlay to send it through; nothing falls back to clearnet |
| `join_refused` | 1 | `toon join` named a network other than the one this agent node was initialised for, the agent node has already joined one, or it records no connector for the network; nothing was spent |
| `relay_not_payable` | 1 | `toon event publish --relay`, `toon message send --relay` or `toon relay subscribe` could not read the relay's information document (or, for a `wss://` relay, its certificate did not verify), or it names no write edge (`toon`: `ilp_address`, `connector_url`, `connector_seal_key`, `price`; the key is 65 bytes of hex beginning `04`) or, for `subscribe`, no subscribe route (`toon_subscription`: `ilp_address`, `price`, `broadcast_price`); nothing was paid |
| `peering_needed` | 1 | `toon event publish --relay`, `toon message send --relay` or `toon relay subscribe` found no route that forwards the relay's `ilp_address`; nothing was paid and nothing was created. If no peering of this agent node reaches the relay's `connector_url`, the message prints the `toon peer add <connector_url> --deposit <amount> --yes` to run (for `subscribe`, with the deposit it would take in place of `<amount>`), then `toon route add`. If one does (the network joined from that URL, or a peering whose id is the label `peer add` derives from it), the message names only `toon route add <address> --peer <id>` and says no new deposit is needed |
| `no_follow_list` | 1 | `toon event query --following` or `toon relay subscribe --following` found no follow list of the agent identity on the agent node's own relay, or one with no keys; nothing was sent or paid |
| `not_confirmed` | 1 | A command that moves money was run without `--yes`, so it did nothing |
| `spending_limit` | 1 | A payment is over the per-command limit or what is left of the day's, or the spending limit is missing or was not signed by the wallet; the message says which limit and how much remains |
| `funds_held` | 1 | `toon destroy` did nothing: a channel of the TOON app still holds funds, or its channels could not be read; the message names each |
| `last_toon_app` | 1 | `toon destroy` was given the only TOON app: an agent node always has one |
| `one_relay` | 1 | `toon create` or `toon add` was given the relay's image (any tag or digest of the repository this build pins): an agent node runs one relay, the one `toon init` created; nothing was changed |
| `describe_failed` | 1 | `toon describe` got no self-description from the connector's `/ilp` URL: it did not answer, answered an error status, or answered something that is not a self-description (a JSON object with `routes`); nothing was paid |
| `agent_key_not_kept` | 1 | `toon message list` found no kept secret of the agent identity in the agent node's home, so no private message has been opened: `toon event publish` and `toon message send` write it; nothing was read or sent |


## The wallet passphrase

`toon init`, `toon wallet show`, `toon wallet balances`, `toon wallet backup`, `toon wallet restore`, `toon relay subscribe`, `toon event publish`, `toon event sign`, `toon message send` and `toon nip publish` read the passphrase from the file named by
`TOON_PASSPHRASE_FILE`, else from `TOON_PASSPHRASE`. It is never a flag. One trailing
newline in the file is not part of the passphrase.

`toon wallet fund` reads the settlement keys the agent node holds, not the keystore, so it
needs no passphrase. `toon relay subscriptions` reads the subscriber key's secret that
`toon relay subscribe` kept in the agent node's home, and needs the passphrase only when that
key is not kept (the file is absent or not a whole key).

## Network profiles and funding

`toon init --network` takes `devnet` (the default), `sandbox` or `mainnet`. Solana
settlement is off unless `--solana` is given. `toon up` does not start a connector whose
settlement key holds less than one whole token, or on Solana less than 0.01 SOL for fees; it
fails with `unfunded`. An EVM connector starts without gas: gas is spent when it sends a
transaction, so `toon join`, `toon peer add` with a deposit, `toon create` with a deposit (both
keys send one) and `toon channel open`, `fund`, `withdraw` and `land` fail with `unfunded` while
the EVM settlement key holds less than 0.001 ETH. They refuse before the spending limit is
charged and before anything is sent. A connector that refuses such a write because the chain
would not estimate the transaction for lack of gas (a 502 whose text says `out of gas`,
`gas required exceeds` or `insufficient funds`, as RPC nodes word it) is reported the same way,
with the connector's text kept: it estimates before it signs, so nothing was sent, and the
deposit is not counted against the spending limit. A chain that cannot be asked is not a verdict: the
connector answers for itself. The devnet faucet sends no ETH: the message then says Base
Sepolia ETH comes from a public Base Sepolia faucet, to be given the address it names, and
does not name `toon wallet fund`. `toon wallet fund` exits 0 when only gas is lacking, and says
so. `needs` of `toon init` and `lacking` of `toon wallet fund` carry `"for"`: `"start"` or
`"deposit"`. Only the devnet has a faucet: on the other networks `toon wallet fund`
fails with `faucet_unavailable`, and on mainnet the operator funds the addresses themselves.

`mainnet`'s profile names a connector, `https://connector.mainnet.toonprotocol.dev/ilp`, and a
relay, `wss://relay.mainnet.toonprotocol.dev`, run by one operator (ADR 0009); `--connector-url`
and `--relay-url` at `init` name others.

`sandbox` allows plaintext peers by itself, since every endpoint of the sandbox is a plain
`http://` one, and its hub is `http://localhost:3200/ilp`. A hidden agent node cannot reach
`localhost`, so `init --network sandbox` without `--clearnet` and without `--connector-url`
records no connector, says so, and `toon join sandbox` fails with `join_refused` until `init`
names the hub: `--connector-url http://<hub>.anyone:3200/ilp` (and
`--relay-url ws://<hub>.anyone:7100`), or `--clearnet`.

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

On a hidden agent node, one with at least one hidden TOON app, the requests a command makes
itself also go through the overlay's SOCKS5 proxy, naming the host: the faucet and chain RPC of
`wallet fund` and `wallet balances`, a relay's information document and balance read
(`event publish --relay`, `relay subscribe`, `relay subscriptions`), a relay's websocket
(`event query`), and the `/identity` and packet of `send --seal-to`. A plain
`http://` or `ws://` endpoint on this machine (`localhost`, `127.0.0.1`, `[::1]`) is dialled
directly, as the connector's own RPC is. If the overlay cannot be had, each of these commands
fails with `overlay_unavailable` and dials nothing directly; a relay the proxy refuses fails as
that command does for a relay that did not answer. Elsewhere, requests are dialled directly.

`toon init --clearnet <hostname>` asks for clearnet instead. The connector binds the
`--listen` address (`127.0.0.1:0` unless given), and the certificate and the reverse proxy
that answer at the hostname are the operator's to provide. A clearnet TOON app needs no
overlay and no terms flag.

The overlay is Anyone's `anon` daemon. `toon` downloads the release pinned in `src/anon.rs`,
checks its SHA-256 and only then runs it, one daemon per agent node home, detached so it
outlives the command that started it and shared by every connector. It stops when the
supervisor does, and after `init` or `create` when no supervisor is running. The operator's agreement
to the terms is recorded in `overlay/agreed` once the daemon has bootstrapped; `toon up` starts
no daemon without it. `TOON_ANON_MIRROR` replaces the download location (the checksum still
applies).

A relay URL is `ws://` or `wss://` wherever one is given, and any other scheme is refused with
the code of the command, the message naming both. `wss://` is dialled with TLS, through the
overlay's proxy too, where the proxy is asked for the host by name and TLS runs inside the
stream it returns; with no port it means 443, and `ws://` means 80. The certificate is checked
against the host name in the URL and the roots compiled into the binary, and a relay's
information document is read over `https://`. A certificate that does not verify fails as the
command does for a relay that did not answer, and the message says it was the certificate.
`TOON_TRUSTED_ROOT`, a seam for tests and private deployments, names a PEM file whose
certificates are trusted as well, for the websocket and the information document alike; a file
that cannot be read or holds no certificate is an error, and it never disables verification.

A hidden service hides where the TOON app is reachable and not who it pays: payments are
on a public chain. `toon init` says so.

## Events

`toon event publish` signs with the agent identity, which is why it needs the passphrase,
and sends the event to the agent node's own relay as an operator write: it exits 1 with
`"outcome": "rejected"` when the packet is rejected, `"outcome": "refused"` when the relay
answers with a status that is not 2xx, and `"outcome": "wrong_fulfilment"` when the packet is
fulfilled but not by this connector. `toon event query` is a plain NIP-01 `REQ` and needs no
passphrase; it reads from `ws://` and `wss://` relays. `toon event sign` signs as `toon event
publish` does and prints the event (alone as text, under `event` with `--json`): it sends
nothing, pays nothing, and does not need the agent node to be running.

`toon event publish --relay <relay-url>` publishes to a relay this agent node does not run. It
reads the relay's NIP-11 information document (`GET` of the relay's URL as `http://`, or `https://` for a `wss://` relay, with
`Accept: application/nostr+json`) for its `toon` object: `ilp_address`, `connector_url`,
`connector_seal_key` and `price`. It seals the packet to `connector_seal_key` and makes no
request to `connector_url`, which only appears in the `peering_needed` message. It shows the
amount and publishes only with `--yes`, under the spending limit, paying from this agent
node's own connector over a peering. If no peering of the agent node reaches
the relay's `ilp_address` it fails with `peering_needed` and creates nothing (when a peering already reaches the `connector_url`, the message asks only for a `route add`). The report has
the same outcomes as a publish to the own relay, plus `relay` and `paid`: the amount sent, or,
when the packet was rejected, what the agent node's outbound channels moved by across it (`0`
when none did). The amount is the relay's price unless `--amount <n>` states
another: `toon` sends exactly `n`, never probes for the path's cost, and refuses an `--amount`
below the relay's price with `usage` before anything is signed. A connector between this agent
node and the relay may charge to forward the write and rejects any other amount than its
route's price (`F03`). A rejected packet whose reject states a cost above 0 is reported with
`cost` (the accumulated cost in base units, a decimal string) and `complete` beside `outcome`,
and the text states the figure as `--amount <cost>`; read the cost from the report. `complete`
is `false` for an `R01`: the packet stopped at a connector it could not pay, `cost` is the
amount to carry to get past that connector, a floor and not the whole cost, and the text says
so instead of naming `--amount`. A cost of 0 is left
out. `reject.message` is the reject's own message. Nothing is retried, and the same report is
given by `toon send`. `--yes` is refused without `--relay`.

`toon relay subscribe <relay-url> --filter <filter> --amount <amount>` subscribes to another
relay's paid live feed (`nips/paid-subscription.md`). It reads `toon_subscription` from the
relay's information document and shows the subscribe price, the broadcast price and the
events the amount buys. It pays only with `--yes`, under the spending limit, over a peering
(`peering_needed` names the deposit it would take and creates nothing). The amount is paid as
whole packets of the packet amount, each authorized by NIP-98 with the subscriber key
(`m/10473'/6'/0'`, not the agent identity). A packet is sent for the subscribe price unless
`--packet-amount <n>` states another: every packet is sent for exactly `n`, `toon` never
probes for the path's cost, and a `--packet-amount` below the subscribe price is `usage`. The
number of packets is `--amount` divided by the packet amount, rounded down, and an `--amount`
that buys none is `usage`. The spending limit is checked against packets times packet amount,
and the day's count rises by `paid`. A connector between this agent node and the relay may
charge to forward and rejects any other amount than its route's price (`F03`); the rejected
packet's `response` then holds `cost` and `complete` as for `toon send`, and the text states
`--packet-amount <cost>`, since the cost is one packet's; read the cost from the report. The report gives
`packets`, `paid` (the packet amount times the packets fulfilled), `credited` (what the relay
answered, less than `paid` through such a connector), `price` (the subscribe price),
`packet_amount`, `balance`, `broadcast_price` and `filter`. A packet the relay refuses still cost
its amount; a rejected or wrongly fulfilled packet adds to `paid` what it moved the outbound
channels by. `outcome` says `refused`, `rejected`, `wrong_fulfilment`, `unanswered` when the connector did
not answer a later packet within the wait (it has expired by then, and adds to `paid` what it
moved the outbound channels by) or, when a later packet could not be sent, `failed`, with the
exit code 1; a packet that failed after it may have left stays counted against the limit at
its amount. A first subscription needs
`--filter`; a later one may leave it out to top up with the filter last kept, or give a new
one to replace the old, and keeps the balance. `toon relay subscriptions` lists, per relay, the
`balance`, `filter` and `subscriber_key`, read from the relay now (`current: true`; a relay
that holds no subscription for the key answers a balance of 0) or as it last answered, and
`exhausted`: whether the balance is below the broadcast price. Each item carries `read_at`,
the Unix second a relay last stated the balance (the answer to a subscribe payment or to a
balance read, a `404` included); for a relay that did not answer it is the earlier time. `toon
status` asks no relay: it lists each subscription under `agent_node.subscriptions` with the
same `exhausted` and `read_at`, as last kept, and its text line says the balance is as last
read, and when, and names `toon relay subscriptions` as the command that reads the current one.
An entry kept before `read_at` existed has `read_at: null`, and its line says the time is unknown.

The supervisor receives every subscription that has a balance (ADR 0005). It dials the
relay's live feed, answers its NIP-42 challenge with the subscriber key (`subscribe` keeps
that key's secret in `subscriber.key` in the agent node's home, for the supervisor, which has
no passphrase), and writes each event to the agent node's own relay at its write endpoint
over loopback, through the overlay's proxy when the agent node has one. It reads
`subscriptions.json` again every moment: after a restart every subscription with a balance
resumes, a top-up resumes one that ran out, and a feed that drops is dialled again. When the
relay closes a feed with `payment-required` the subscription is marked exhausted until it is
topped up.

`--following`, on `toon event query` and `toon relay subscribe`, sets the `authors` of the filter
to the keys in the `p` tags of the newest kind 3 event the agent identity signed, read from the
agent node's own relay (which must be running, else `not_running`). `--filter` is then optional
and keeps its other fields; one that already has `authors` is `usage`, and no follow list, or
one without keys, is `no_follow_list`, each before anything is sent or paid. The filter a
subscription is given is a snapshot: following someone later does not change it, and the
report of `relay subscribe --following` says how many keys it holds and that it stays fixed
until the command is run again. The command needs the passphrase, to name the agent identity.

`toon event watch [--filter <json>] [--following]` prints the live events of the agent node's own
relay, one JSON document to a line, flushed as each arrives, with or without `--json`. It takes no
relay URL, needs no subscription, sends no packet and counts nothing against the spending
limit. Only events that arrive after it starts are printed; `event query` reads the stored
ones. `--filter` is parsed and refused as on `event query`, and its `limit` is dropped;
`--following` sets its `authors` as on `event query`, read once at the start. A relay that
sells its feed keeps a `REQ` open only for its operator, so the command answers the relay's
NIP-42 challenge with the relay's own identity key, signing the `relay` tag with the URL the
relay is reached at (its onion endpoint or public hostname) and not the loopback address it
dials; a relay that does not sell its feed sends no challenge and is read at once. Without an
agent node it fails with `no_agent_node`, with no relay in it `unknown_name`, and with the relay not
running `not_running`. A feed has no end, so it exits 1 with the reason it stopped:
`query_failed` when the relay closed the feed or dropped. `toon event follow` is removed and is
`usage`, naming `toon event watch`.

`toon nip publish` signs a draft (`nips/proposals-as-events.md`) the same way and writes it
to the agent node's own relay as `toon event publish` does, with the same outcomes. It first
asks `--relay` for the draft's current revision, so `--relay` must be the agent node's own
relay's `ws://` or `wss://` URL. It exits 1 with `draft_refused` when the file is not a
draft it can publish, or the relay holds that identifier under another title and `--title-changed` was
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
`g.toon.<segment>.<name>`. `--image` or `--url` says which app; `--clearnet`, `--accept-anyone-terms` and
`--listen` say how the connector is reached, as for `toon init`. It reads the wallet passphrase.
It is peered with `<from>`, the first TOON app if `--app` is left out, in both directions,
each channel opened with `--deposit`, with a forwarding route each way; `--no-peer` says not to.
The deposits move money, so they need `--yes` and count twice against the spending limit. A
settlement key that holds too little fails with `unfunded`, and nothing is created: the message
names the address to fund. With a deposit, both the new app's key and `<from>`'s must hold the
gas a deposit spends (0.001 ETH on EVM); with `--no-peer` neither needs any. A running supervisor starts the connector; otherwise `toon up` does.

`--app <name>` on any command that talks to a connector, such as `toon send`, `toon peer`,
`toon route` and `toon channel`, says which TOON app it is about; the first TOON app is the
default.

`toon destroy <name>` stops a TOON app's connector and removes its files and its app's data,
except an onion key. It needs the TOON app running, to read its channels, and fails with
`funds_held`, naming each channel, while an outbound channel holds collateral or an inbound one
a voucher that has not landed. It never removes the last TOON app (`last_toon_app`).

## The spending limit

Every command that moves money (`toon send`, `toon peer add`, `toon join`) states its amount and needs
`--yes`. `toon probe` moves money only for an amount: with the default amount of 0 it needs no `--yes`, is
not checked against the limits and counts nothing against the day, and its report carries `paid: 0`; with
any other amount it is counted as `toon send` is, by what the outbound channels moved by. The amount is checked against the spending limit before the command runs: at most
`--max-per-command` for one command, and `--max-per-day` for the commands of one UTC day
together, both in the token's base units, set at `toon init` (defaults 10000000 and
100000000). A packet that was rejected is counted by what the watermarks of the agent node's
outbound channels moved by across it, read before the packet is sent and after it is
answered, under a lock shared by the packet-sending commands. `paid` in the JSON report of
`toon send`, `toon event publish --relay` and `toon relay subscribe` carries that amount: `0`,
and not counted, when the agent node's own connector rejected the packet before signing
anything, and possibly the packet's whole amount when a connector farther on rejected it (the
outbound watermark in `toon channel list` shows the same). If the watermarks cannot be read,
the packet's full amount stays counted. Nor is a payment counted that failed before it reached
the connector or that the other side refused. A peering the connector refused because both
connectors settle on more than one chain and no `--chain` named one (`peer_failed`, before any
channel is opened) is not counted either, for `peer add`, `join` and `create --deposit`. A
packet that was never sent is not counted either: the operator key or the `--body` file of `toon send` could not be read, the identity
of the connector to seal to could not be fetched or has no usable public key, the packet could not be sealed, or the
connector's `send` refused its arguments. These still fail with `send_failed`. Any other
failure may have paid, and stays counted, including a refusal from the operator surface and
an answer that was not understood. A packet the connector did not answer within the wait,
which is longer than the packet's 30-second expiry so that the connector's own reject is
what is normally reported (a connector forwarding it answers `R00` at the packet's outgoing
expiry, a little before its own, and signs nothing for a packet that has run out of time), fails with `send_failed` and is
counted by what the watermarks moved by, like a rejected packet (the whole amount, if they
cannot be read). A packet the next hop never carried is not paid for on a batch-settlement
channel when the next hop can be asked where it stands: the next forward signs from what it
reports, so the count can be above what the packet finally costs. The failure's
JSON is `{"error": {"code", "message"}, "paid"}` with the `event` for `toon event publish
--relay`; the text says that the packet has expired, names the event's id so that
`toon event query` can ask the relay for it, and gives the "It cost N base units." sentence
when N is above 0. `toon limit show` prints the limits and what is left
today. `toon limit set` changes them and reads the wallet passphrase, so an agent without it
cannot raise them: the limits are signed with a key the wallet derives, and an unsigned or
edited `limits.json` stops every payment. When `limits.json` is missing or was edited,
`toon limit set` needs both `--max-per-command` and `--max-per-day`.
