# toon

`toon` runs an **agent node**: a machine that sells HTTP services for money and pays other
machines for theirs, packet by packet, without the services themselves knowing anything
about payments. One binary sets up the wallet, runs the connectors that charge and forward
packets, runs the apps behind them, and peers with other operators.

```sh
toon init --accept-anyone-terms     # a wallet, and a relay behind a paid connector
toon wallet fund                    # devnet tokens from the faucet
toon up                             # start it, as a systemd --user unit
toon status                         # what is running, and at which address
```

## Why it exists

An agent that does useful work should be able to charge for it, and an agent that needs
work done should be able to pay for it, without either one writing payment code. TOON
splits the two jobs:

- An **app** is a plain HTTP service. It answers requests and never sees a payment.
- A **connector** sits in front of it. It takes a packet, checks it is paid, delivers the
  request inside it to the app, and returns the answer. Payment is settled on chain through
  channels the connectors open toward each other.

`toon` is the operator's tool for that arrangement. It is built for an operator that is
often an agent itself, so it is hard to misuse:

- **Nothing moves money by accident.** Every command that pays states its amount, needs
  `--yes`, and is refused past a spending limit that only the wallet passphrase can raise.
- **Nothing is published by accident.** A new connector is a hidden service on the Anyone
  overlay. If the overlay will not come up, `toon` fails instead of falling back to a public
  address.
- **Nothing is interrupted by accident.** A command that restarts a running connector needs
  `--yes`, and a TOON app whose channels hold funds cannot be destroyed.
- **Nothing needs a human at the keyboard.** No command prompts. Every command takes
  `--json` and prints exactly one JSON document, and exit codes and error codes are stable
  ([`docs/exit-codes.md`](docs/exit-codes.md)).

## How it fits together

```
 agent node: one machine, one wallet, one supervisor
 ┌────────────────────────────────────────────────────────────────┐
 │  TOON app "relay"                                              │
 │    connector  g.toon.<segment-a>  ──►  app "relay" (Nostr)     │
 │                                                                │
 │  TOON app "search"                                             │
 │    connector  g.toon.<segment-b>  ──►  app "search"            │
 │                                   └─►  app "notes"             │
 └───────────────▲──────────────────────────────┬─────────────────┘
                 │       peerings: paid         │
                 │       channels between       ▼
            other operators' connectors, and the network's hub
```

A **TOON app** is one connector and the apps behind it. Every agent node starts with one,
whose only app is a Nostr relay. Read [Concepts](docs/guide/concepts.md) for the rest of
the vocabulary; [`CONTEXT.md`](CONTEXT.md) is the full glossary.

## Guides

| Guide | What it walks through |
| --- | --- |
| [Concepts](docs/guide/concepts.md) | Agent node, TOON app, connector, app, peering, ILP address, and why each exists |
| [Install](docs/guide/install.md) | A release binary or a build from source, upgrading, and the agent skills |
| [Your first agent node](docs/guide/first-agent-node.md) | `init`, fund, `up`, publish an event to your own relay, read it back |
| [Money: wallet, limits and channels](docs/guide/money.md) | Funding, balances, the spending limit, `--yes`, and managing channels |
| [Peering](docs/guide/peering.md) | Joining a network, peering with another operator, routes, and sending a packet |
| [Adding an app to a connector](docs/guide/adding-an-app.md) | Writing an app, `toon add`, prices, and taking an app away |
| [Creating a TOON app](docs/guide/creating-a-toon-app.md) | A second connector with its own keys: `toon create`, `toon destroy` |
| [The relay](docs/guide/relay.md) | Publishing and reading events, relay settings and prices, selling and buying a live feed |
| [Networks and reachability](docs/guide/networks-and-reachability.md) | devnet, sandbox and mainnet; hidden service or clearnet |
| [Running it day to day](docs/guide/operations.md) | `up`, `down`, `status`, `logs`, scripting with `--json`, and what an error code means |
| [Backup and restore](docs/guide/backup-and-restore.md) | Sealing the wallet, restoring it, and what a mnemonic alone does not keep |

## Reference

- `toon --help` and `toon <command> --help`: the commands of the binary you have.
- [`docs/exit-codes.md`](docs/exit-codes.md): output rules, exit codes, every error code.
- [`docs/adr/`](docs/adr/): the decisions behind the design, and why.
- [`nips/`](nips/): the protocol drafts that ship with the CLI, such as the paid
  subscription.
- [`docs/development.md`](docs/development.md): building, the checks, releasing, and the
  end-to-end run.
