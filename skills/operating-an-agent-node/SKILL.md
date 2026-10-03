---
name: operating-an-agent-node
description: "Operate an agent node with the `toon` CLI: set up the wallet, fund it, add and create TOON apps, peer, set routes and prices, find out what another connector offers before paying it, manage channels, configure the relay, send packets, back up and restore. Use when asked to run, change or inspect an agent node, or to pay anything through it."
---

# Operating an agent node

This skill ships inside the `toon` binary and describes the commands of that binary. If a
command here is missing from `toon --help`, the skill is out of date: trust `--help`.

## Terms

- **Agent node**: everything one operator runs through the CLI on one machine: one wallet and
  one or more TOON apps. Always two words.
- **TOON app**: one connector together with the apps behind it. **App**: the service alone, a
  payment-oblivious HTTP service the connector delivers to. The relay is an app. Never call an
  app a "TOON app": a TOON app is a connector with its apps, an app is the service alone.
- **Connector**: the paid reverse proxy at the front of a TOON app.
- **Supervisor**: the one process per machine that runs the agent node and its connectors.
- **Operator**: whoever runs the agent node. That is you.
- **Wallet**, **spending limit**, **agent identity**: see below. The agent identity is a Nostr
  key that is not a payment key.
- **Hidden service**: a connector reachable only at its onion endpoint, a `.anyone` address.
  **Clearnet** is an ordinary public hostname, and is never the default.

## How every command behaves

- Pass `--json` and read the one JSON document on standard output. Success and failure both
  print one; a failure is `{"error": {"code": ..., "message": ...}}`. Branch on `code`, never on
  `message`.
- Exit codes: 0 done, 1 failed (the error code says why), 2 command line not understood,
  3 no agent node on this machine. `toon --help` lists them; the error codes to branch on are
  named where they arise below.
- No command prompts or reads standard input. The wallet passphrase comes from the file named by
  `TOON_PASSPHRASE_FILE`, else from `TOON_PASSPHRASE`, never from a flag.
- `--app <name>` says which TOON app a command about a connector is about; the first one is the
  default. Amounts are in the token's base units.
- Ask for help with `toon --help` or `toon <command> --help`; there is no `help` subcommand.

## Money: read this before running anything

These commands spend, and each one needs an explicit amount and `--yes`:

- `toon send <address> --amount <n> --yes`
- `toon peer add <address> --deposit <n> --yes`
- `toon join <network> --deposit <n> --yes`
- `toon create <name> --deposit <n> --yes` (the two channels count twice against the limit)
- `toon event publish --relay <ws-url> --yes` (pays the price the relay states; add `--amount <n>` when a connector in between charges to forward)
- `toon message send <pubkey>... --content <text> --relay <ws-url> --yes` (pays the relay's price for each recipient's wrap)
- `toon relay subscribe <ws-url> --filter <json> --amount <n> --yes` (prepays a subscription at another relay: the balance its live feed draws down; `--packet-amount <n>` when a connector in between charges to forward)

Without `--yes` nothing moves and the command fails with `not_confirmed`. Never add `--yes` to
see what a command would do: run it without, or read the price first.

The **spending limit** is the most the wallet pays out per command and per UTC day. A payment
over either is refused with `spending_limit` before anything is sent, and the message says
which limit and how much of the day remains. `toon limit show` prints the limits and what is
left today. `toon limit set --max-per-command <n> --max-per-day <n>` changes them and needs the
passphrase. Do not raise the limit to get past a refusal unless the operator who gave you this
task said to; report the refusal instead. A packet that was rejected costs what your
connector's outbound channels moved by across it: possibly its whole amount, shown as `paid` in
the report, counted against the limit, and visible as the outbound `watermark` in
`toon channel list`. A packet your own connector rejected before signing costs nothing. A
payment whose packet was never sent is not counted either (the key could not be read, the
identity to seal to could not be fetched, the packet could not be sealed, or the connector's `send` refused its
arguments); any other failure stays counted, because the packet may have left.

`toon channel open` and `toon channel fund` also put collateral in a channel; they take no
`--yes`, so state the deposit deliberately and check `toon limit show` and `toon wallet balances`
first.

Commands that restart a connector (`toon add`, `toon remove`, `toon route price`,
`toon relay config`, `toon relay price`) drop the packets it holds. They need `--yes` once the
connector is running, or fail with `confirmation_required` and change nothing.

## Setup

1. `toon init --network devnet --accept-anyone-terms` creates the wallet and the first TOON app,
   the relay TOON app, as a hidden service. The mnemonic is shown once: record it where the
   operator keeps secrets. `--network` is `devnet` (default), `sandbox` or `mainnet`.
   `mainnet` names one operator's node (Drew's, `g.drew`) as its connector and relay;
   `--connector-url` and `--relay-url` on `init` name others. `sandbox` allows plaintext peers by itself; a
   hidden agent node on it records no connector, and names the sandbox's hub with
   `--connector-url http://<hub>.anyone:3200/ilp` (and `--relay-url ws://<hub>.anyone:7100`),
   else `toon join sandbox` is refused (`join_refused`).
   `--clearnet <hostname>` asks for clearnet instead and needs no terms flag; the certificate and
   reverse proxy are yours to provide. If the overlay will not bootstrap, `init` fails with
   `overlay_unavailable` and never falls back to clearnet. On a hidden agent node the requests
   the commands make themselves (`wallet fund`, `wallet balances`, `event publish --relay`,
   `event query`, `relay subscribe`, `relay subscriptions`, `send --seal-to`) go
   through the overlay too, except to a plain `http://` or `ws://` endpoint on this machine, and
   fail with `overlay_unavailable` when it is not there. `--max-per-command` and `--max-per-day`
   set the spending limit. `--solana` adds Solana as a settlement chain. The settlement chains
   are chosen at `init`; no command adds one later, so decide before running it.
2. Fund the wallet (next section).
3. `toon up` starts the supervisor as a `systemd --user` unit. `toon up --foreground` runs it in
   the current process. `toon down` stops it.
4. `toon status` reports the agent node, each TOON app and its apps, and how often the supervisor
   restarted a connector. `toon logs <name>` shows the log of a TOON app or an app.
5. `toon wallet show` lists the addresses by chain and the agent identity.

## Funding

`toon up` fails with `unfunded` while a settlement key holds less than one whole token (on Solana,
also 0.01 SOL for fees); the message names each address and the amount. An EVM connector starts
without gas. Gas (0.001 ETH) is needed where it is spent: `toon join`, `toon peer add` with a
deposit, `toon create` with a deposit and `toon channel open`, `fund`, `withdraw` and `land` fail
with `unfunded` without it, before anything is charged or sent. A connector that refuses a
write because the chain would not estimate it for lack of gas is reported as `unfunded` too: nothing was
sent and nothing is counted.

- On the devnet, `toon wallet fund` asks the faucet. It sends the token and no ETH: Base Sepolia
  ETH comes from a public Base Sepolia faucet, which you cannot use, so say so and stop. It asks for
  every address whatever the faucet answers for the others: a refusal for one address (an EVM
  cooldown, say) does not stop the rest. The report lists each as funded or refused with the
  faucet's reason; the command succeeds if any was funded, and fails with `faucet_unavailable`
  naming each address when none was.
- On `sandbox` and `mainnet` there is no faucet (`faucet_unavailable`): the operator sends funds
  to the addresses `toon wallet show` lists. You cannot do that, so say so and stop.
- `toon wallet balances` shows the balance of every address by TOON app and chain.

## Add an app: `toon add`

`toon add <app> --to <toon-app> --image <image>` puts a new app behind the connector of a TOON
app that exists already; `--url <url>` names an app you already serve and runs nothing. `--price`
is what a client pays the connector for a packet to the app, `--address` the ILP prefix
(`g.toon.<segment>.<app>` by default). It restarts that connector, so it needs `--yes`. `toon remove <app>`
takes an app and its route away. An agent node has one relay, the one `toon init` made: `add`
refuses the relay's image (any tag or digest) with `one_relay`.
`--request <file>` takes a file holding one JSON object that says what a client should send the
app; the connector publishes it, unread, as the route's `request` on `GET /ilp` and `toon route
list`. Example: `toon add echo --to relay --image echo-app --request request.json --yes`, with
`{"protocol": "http", "method": "POST", "path": "/"}` in `request.json`. A file that is not one
JSON object, or holds `null`, is refused with `usage`. To change it, remove the app and add it again.

An image given with `--image` must:

- listen on port 3100 inside the container (`TOON_BLS_PORT`),
- answer `GET /health` with `200` within two minutes of starting,
- keep what it must not lose under `/data` (`TOON_DATA_DIR`).

The app is given no environment beyond `TOON_BLS_PORT` and `TOON_DATA_DIR`. An image that needs
a secret or another variable to start fails with `app_failed`; `toon logs <app>` says why. Do not
assume a published image meets this: the one image known to run unchanged is the minimal app in
[Adding an app](https://github.com/toon-protocol/toon_cli/blob/main/docs/guide/adding-an-app.md),
built with `docker build`; the end-to-end run adds an app built the same way.

An app's loopback address is not a way to pay it. A request made straight to it does not arrive
as a packet through the connector, so it is unpaid work: never do it in place of a payment.

## Create a TOON app: `toon create`

`toon create <name> --app <from> --image <image>` starts a new TOON app: a new connector with its
own identity and keys, and an app behind it. It peers with `<from>` in both directions, each
channel opened with `--deposit`, so it moves money (see above); `--no-peer` creates no peerings
and needs no deposit. It refuses the relay's image with `one_relay`, as `add` does. `--clearnet`
and `--accept-anyone-terms` work as for `init`.
`toon destroy <name>` stops and removes a TOON app, and refuses with `funds_held` while a
channel still holds funds; the last TOON app is never removed.

## Peering

A **peering** is one connector's standing arrangement to forward packets to another, paid on a
channel it opens toward it. Yours alone forwards nothing back: the other connector forwards
back only if its operator creates a peering in return.

- `toon peer add <address> --deposit <n> --yes` peers toward the `/ilp` URL of another connector.
  `--id` labels it, `--fee` is what you keep of each packet forwarded, `--max-packet-amount` caps
  one packet. `peer_not_peerable` means the refusal is on the other side. `--chain evm|solana`
  names the settlement chain; it is needed when both connectors settle on more than one (an
  agent node with `--solana`). Without it the connector refuses, `peer_failed` says to run again
  with `--chain`, and nothing is counted against the limit. `join` and `create --deposit` take it too.
- `toon describe [<ilp-url>]` prints what a connector offers before you pay it: its addresses,
  settlement terms, and each route with its price and whether it states a `request`. With
  `--json` the self-description is unaltered under `description`. Without a URL it describes your
  own connector (`--app` chooses which). It pays nothing, and fails with `describe_failed`.
- `toon peer list` shows the peerings and their labels; `toon peer remove <id>` removes one.
- `toon join <network> --deposit <n> --yes` peers toward the network's own connector and reads
  its relay. The network must be the one `init` was given; it is done once.

## Routes and prices

- `toon route list` shows the routes the connector terminates (to an app) and forwards (to a peer).
- `toon route add <prefix> --peer <id>` forwards an ILP address prefix to a peering. `--price` is
  what a client pays on that route.
- `toon route price <prefix> <price>` sets what a client pays for a packet to an app.
- `toon route remove <prefix>` stops forwarding a prefix.

## Channels

`toon channel list` shows both directions with collateral and status. `toon channel open --terms
<file> --deposit <n>` opens an outbound channel from the counterparty's `batchSettlements` entry.
`toon channel fund <id> --amount <n>` adds to an outbound channel. `toon channel withdraw <id>`
starts or finishes withdrawing collateral. `toon channel land <id>` lands the latest voucher on an
inbound channel now.

## Relay settings

`toon relay config` shows the relay's settings and prices. With `--name`, `--description`,
`--expiry honour|ignore`, `--block <event-id>` or `--unblock <event-id>` it changes them and restarts
the relay and its connector (`--yes`). `toon relay price <amount>` sets the price of one write
to the relay's route.

To sell the relay's live feed (ADR 0005), `toon relay price --subscribe <amount> --broadcast <amount>`
sets what a subscribe packet costs and credits, on a new route of the connector, and what the relay
debits for each event it broadcasts to a subscriber. The two come together, and a price of `0` stops
selling. `toon relay subscriptions --incoming` asks the running relay who subscribed and what each
has left. The counts are in `toon status` (subscriptions held with a balance and exhausted, and subscriber
keys of its own relay with a balance, or unknown while the relay does not answer), and in `totals` of
`toon relay subscriptions` and of `toon relay subscriptions --incoming` with `--json`.

## Finding out what a connector offers

Before you send anything to a connector that is not yours, find out what it offers. Do it in
this order; the first two steps pay nothing.

1. Read its self-description: `toon describe <ilp-url> --json`, where `<ilp-url>` is the
   connector's `/ilp` URL. It gives the addresses the connector answers to, how it settles and
   its routes. Without a URL it describes your own connector.
2. Read each route's price and its `request`, which says what to send it (method, path and so
   on). A route with no `request` does not say what to send: do not guess a method, a path or a
   body, and tell the operator that the route states none.
3. Pay with a request: `toon send <address> --amount <n> --seal-to <ilp-url> --method <method>
   --path <path> --body <file> --yes` (see "Paying an app"). Never call the app on a loopback
   address: that is unpaid work, not a payment.

The price you read in step 2 is that connector's price alone. A connector on the way to it may
charge to forward. A packet sent with `toon send` for the destination's price and rejected `F03`
met such a connector. The report of the rejected packet says what the path costs: read `cost`
(base units, a string) and send it as the amount. If `"complete": false` (an `R01`) the cost is a
floor, the amount to get past the connector that stopped the packet, and not the whole cost. Do
not guess an amount, and do not raise the amount step by step until a send is fulfilled.

If you are the operator publishing an app, declare its `request` when you add it
(`toon add <app> --to <toon-app> --request <file>`, above), so that others can do step 2.

## Sending

`toon send <address> --amount <n> --yes` sends one packet and reports fulfilled or rejected. It
exits 1 when the packet was rejected, and the reject code is in `reject.code`; `paid` is what it
cost, which for a rejected packet may still be its whole amount; it also exits 1
with `"outcome": "wrong_fulfilment"` when the fulfilment does not match. `--seal-to <url>` seals
the payload to the identity of a connector that is not yours.

`toon event publish --kind <n> --content <text>` signs an event with the agent identity and
writes it to your own relay; `toon event query <ws-url> --filter <json>` reads a relay. Publishing
to someone else's relay is `toon event publish --relay <ws-url> --yes` and needs a peering that
reaches that relay's connector, or it fails with `peering_needed` and pays nothing. It sends
the relay's price; if a connector in between charges to forward and rejects the write with
`F03`, read `cost` from the rejected packet's report and state it with `--amount <cost>` (below
the relay's price is refused); treat `"complete": false` as a floor, not the whole cost, and do
not raise the amount step by step.
`toon relay subscribe <ws-url> --filter <json> --amount <n> --yes` buys the live feed of another
relay: a prepaid balance at that relay, with one filter, drawn down for each event it sends.
The supervisor writes those events into your own relay (ADR 0005). It needs a peering too, and
without `--yes` it fails with `not_confirmed` and says what the relay charges. `toon relay
subscriptions` shows the balance. It is not the follow list, which is an event you publish. It is
the same as publishing in one way: if a connector in between rejects its packets with `F03`,
read `cost` from the rejected packet's report (`response.cost`) and state it with
`--packet-amount <cost>`, treating `"complete": false` as a floor; `--amount` stays the total,
paid as whole packets of that amount, and each packet still credits only the subscribe price.

### Private messages

`toon message send <pubkey>... --content <text>` sends a private message from the agent identity
and, like `event publish`, opens the keystore, so it needs the passphrase;
`--relay <ws-url> --yes` sends the recipients' wraps to another relay and pays its price.
`toon message list` is free: it needs no passphrase and reads what the supervisor opened. It
fails with `agent_key_not_kept` on an agent node that keeps no agent secret yet (one made before
the secret was kept, until a command opens the keystore). The `social` skill's
`references/nip-17.md` has the details.

### Paying an app

An app you added is used by paying it, not by calling its loopback address, which does the work
unpaid. `toon send <app address> --amount <n> --method <method> --path <path> --body <file> --yes`
carries one request in one paid packet: `--method` is `POST` and `--path` is `/` when absent, and
`--path` is the path and query the app receives. `--body` is a file whose bytes are the body, sent as
`application/json`; with none, there is no body. The report's `response` has the app's status and
body. A body file that cannot be read fails before anything is sent and costs nothing.

## Backup and restore

- `toon wallet backup --out <file>` seals the mnemonic and every onion endpoint's address key under
  the passphrase. The file must not exist. Keep the passphrase apart from the file.
- `toon wallet restore <file>` recreates the wallet on a machine with no wallet, then `toon init`
  makes the TOON app on the same keys, at the same onion endpoints.
- `toon init --from-mnemonic` restores from a mnemonic alone (`TOON_MNEMONIC_FILE`, else
  `TOON_MNEMONIC`). The hidden service then gets new onion endpoints, because a mnemonic does not
  hold the address keys.

## The hidden service

A new TOON app is a hidden service: its connector listens on loopback only and is reachable at its
onion endpoint, a `.anyone` address, and all its outbound traffic, settlement RPC included, goes
through the overlay. The endpoint is stable across restarts and is restored by a backup. It hides
where the TOON app is reachable, not who it pays: payments are on a public chain. Clearnet is only
for an operator who asks for it with `--clearnet`.

## Installing this skill

`toon skill install` copies the skills shipped in the binary to `~/.claude/skills`, or to the
directory given with `--dir`. It is safe to run again after an upgrade: it replaces what it
installed with the version in the binary.
