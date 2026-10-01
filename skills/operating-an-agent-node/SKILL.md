---
name: operating-an-agent-node
description: Operate an agent node with the `toon` CLI: set up the wallet, fund it, add and create TOON apps, peer, set routes and prices, manage channels, configure the relay, send packets, back up and restore. Use when asked to run, change or inspect an agent node, or to pay anything through it.
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
- `toon event publish --relay <ws-url> --yes` (pays the price the relay states)

Without `--yes` nothing moves and the command fails with `not_confirmed`. Never add `--yes` to
see what a command would do: run it without, or read the price first.

The **spending limit** is the most the wallet pays out per command and per UTC day. A payment
over either is refused with `spending_limit` before anything is sent, and the message says
which limit and how much of the day remains. `toon limit show` prints the limits and what is
left today. `toon limit set --max-per-command <n> --max-per-day <n>` changes them and needs the
passphrase. Do not raise the limit to get past a refusal unless the operator who gave you this
task said to; report the refusal instead. A payment that was rejected is not counted.

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
   `--clearnet <hostname>` asks for clearnet instead and needs no terms flag; the certificate and
   reverse proxy are yours to provide. If the overlay will not bootstrap, `init` fails with
   `overlay_unavailable` and never falls back to clearnet. `--max-per-command` and `--max-per-day`
   set the spending limit.
2. Fund the wallet (next section).
3. `toon up` starts the supervisor as a `systemd --user` unit. `toon up --foreground` runs it in
   the current process. `toon down` stops it.
4. `toon status` reports the agent node, each TOON app and its apps, and how often the supervisor
   restarted a connector. `toon logs <name>` shows the log of a TOON app or an app.
5. `toon wallet show` lists the addresses by chain and the agent identity.

## Funding

`toon up` fails with `unfunded` while a settlement key holds less than 0.0001 ETH (0.01 SOL) for
gas and one whole token; the message names each address and the amount.

- On the devnet, `toon wallet fund` asks the faucet.
- On `sandbox` and `mainnet` there is no faucet (`faucet_unavailable`): the operator sends funds
  to the addresses `toon wallet show` lists. You cannot do that, so say so and stop.
- `toon wallet balances` shows the balance of every address by TOON app and chain.

## Add an app: `toon add`

`toon add <app> --to <toon-app> --image <image>` puts a new app behind the connector of a TOON
app that exists already; `--url <url>` names an app you already serve and runs nothing. `--price`
is what a client pays the connector for a packet to the app, `--address` the ILP prefix
(`g.toon.<app>` by default). It restarts that connector, so it needs `--yes`. `toon remove <app>`
takes an app and its route away.

## Create a TOON app: `toon create`

`toon create <name> --app <from> --image <image>` starts a new TOON app: a new connector with its
own identity and keys, and an app behind it. It peers with `<from>` in both directions, each
channel opened with `--deposit`, so it moves money (see above); `--no-peer` creates no peerings
and needs no deposit. `--clearnet` and `--accept-anyone-terms` work as for `init`.
`toon destroy <name>` stops and removes a TOON app, and refuses with `funds_held` while a
channel still holds funds; the last TOON app is never removed.

## Peering

A **peering** is one connector's standing arrangement to forward packets to another, paid on a
channel it opens toward it. Yours alone forwards nothing back: the other connector forwards
back only if its operator creates a peering in return.

- `toon peer add <address> --deposit <n> --yes` peers toward the `/ilp` URL of another connector.
  `--id` labels it, `--fee` is what you keep of each packet forwarded, `--max-packet-amount` caps
  one packet. `peer_not_peerable` means the refusal is on the other side.
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
`--expiry honour|ignore`, `--block <pubkey>` or `--unblock <pubkey>` it changes them and restarts
the relay and its connector (`--yes`). `toon relay price <amount>` sets the price of one write
to the relay's route.

## Sending

`toon send <address> --amount <n> --yes` sends one packet and reports fulfilled or rejected. It
exits 1 when the packet was rejected, and the reject code is in `reject.code`; it also exits 1
with `"outcome": "wrong_fulfilment"` when the fulfilment does not match. `--seal-to <url>` seals
the payload to the identity of a connector that is not yours.

`toon event publish --kind <n> --content <text>` signs an event with the agent identity and
writes it to your own relay; `toon event query <ws-url> --filter <json>` reads a relay. Publishing
to someone else's relay is `toon event publish --relay <ws-url> --yes` and needs a peering that
reaches that relay's connector, or it fails with `peering_needed` and pays nothing.

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
