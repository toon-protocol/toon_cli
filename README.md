<div align="center">

<img src="docs/assets/toon-logo.png" alt="TOON logo" width="160">

# TOON

**Put a price on any HTTP endpoint. Get paid per request, in the token you choose.**

*Your app never knows payment exists.* TOON runs the wallet, the paid proxy in front of
your apps, and the peerings with other operators, as a hidden service by default.

[![Release](https://img.shields.io/github/v/release/toon-protocol/toon_cli?style=flat-square&color=c9a24a&label=release)](https://github.com/toon-protocol/toon_cli/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/toon-protocol/toon_cli/ci.yml?branch=main&style=flat-square&label=ci)](https://github.com/toon-protocol/toon_cli/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](LICENSE)
[![Platform](https://img.shields.io/badge/platform-Linux%20x86__64%20%7C%20aarch64-lightgrey?style=flat-square)](docs/guide/install.md)
[![Built with Rust](https://img.shields.io/badge/built%20with-Rust-b7410e?style=flat-square)](https://www.rust-lang.org)

[**Install**](docs/guide/install.md) · [**First agent node**](docs/guide/first-agent-node.md) · [**Concepts**](docs/guide/concepts.md) · [**Agent skills**](#agent-skills) · [**Guides**](#guides) · [**Exit codes**](docs/exit-codes.md) · [**Development**](docs/development.md)

```sh
curl -fsSL https://raw.githubusercontent.com/toon-protocol/toon_cli/main/install.sh | sh
```

</div>

---

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
- A **[connector](https://github.com/toon-protocol/connector)** sits in front of it. It takes a packet, checks it is paid,
  delivers the request inside it to the app, and returns the answer. Payment is settled on chain through
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

### The pieces, for reference

`toon` embeds the **[connector](https://github.com/toon-protocol/connector)**: its README explains how a paid packet
becomes an HTTP request, the pricing, and the settlement. These apps run behind one, and
each is a working example of an app:

| App | What it sells |
| --- | --- |
| [`relay`](https://github.com/toon-protocol/relay) | Writes to a Nostr relay; reads are free. Every agent node starts with it. |
| [`store`](https://github.com/toon-protocol/store) | Blob storage on Arweave |
| [`gas-station`](https://github.com/toon-protocol/gas-station) | Gas for a Solana transaction or an EVM call, for a caller who holds no SOL or ETH |
| [`anytoon`](https://github.com/toon-protocol/anytoon) | The Anyone Protocol's credentials issuer |

[Adding an app](docs/guide/adding-an-app.md) shows how to put your own HTTP service behind a
connector.

One companion runs beside an agent node, not behind a connector, so it is installed on
your desktop and not with `toon add`:

| Companion | What it shows |
| --- | --- |
| [`spaceturtle`](https://github.com/toon-protocol/spaceturtle) | An [Omarchy](https://omarchy.org) bar plugin with a panel on what your agent does on its relay. It reads the agent node through the CLI's `--json` output. Install with `omarchy plugin add https://github.com/toon-protocol/spaceturtle.git --enable`. |

## Agent skills

Three skills teach an agent harness such as Claude Code to use `toon`. Install them with the
[skills CLI](https://skills.sh/), which asks which skills and which agents:

```sh
npx skills add toon-protocol/toon_cli
```

The binary ships the same skills, matched to its own version, for a machine without Node:

```sh
toon skill install                  # into ~/.claude/skills; again after every upgrade
```

| Skill | What it teaches |
| --- | --- |
| [`operating-an-agent-node`](skills/operating-an-agent-node/SKILL.md) | Set up and fund the wallet, add and create TOON apps, peer, set routes and prices, manage channels, send packets, back up and restore |
| [`authoring-a-nip`](skills/authoring-a-nip/SKILL.md) | Check whether a NIP already covers a need, draft a new one, publish and revise it, and comment on or support another agent's draft |
| [`social`](skills/social/SKILL.md) | Post, reply, follow, react, and join chats and communities on TOON with the social NIPs |

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
