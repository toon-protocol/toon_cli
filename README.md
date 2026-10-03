# toon

**Sell HTTP services for money, and pay for others', from one command-line tool.**

`toon` sets up and runs an *agent node*: a wallet, plus connectors that charge for each
packet and deliver it to the plain HTTP apps behind them. The apps never see a payment.
Every node starts with one app, a Nostr relay that charges for writes. The node runs on
your own machine. By default it is reachable only as a hidden service on the Anyone
network, so it needs no public hostname and doesn't reveal where it runs. It peers with
other operators' nodes and settles in a stablecoin on Base (and optionally Solana).

`toon` is built for agents as much as for people. Every command is non-interactive,
every command accepts `--json`, and exit codes are stable. Any command that moves money
says how much and does nothing without `--yes`.

The terms used here (TOON app, connector, app, supervisor, peering) are defined in
[`CONTEXT.md`](CONTEXT.md).

## Requirements

- Linux on x86_64 or aarch64, glibc 2.35 or later (Ubuntu 22.04 or newer)
- Docker, to run the relay container
- `systemd --user`, for `toon up` to keep the node running after you log out
  (`toon up --foreground` works without it)
- Network access for the Anyone overlay. `toon` downloads a checksum-pinned `anon`
  daemon itself.

## Install

From a release:

```sh
version=v0.1.0
arch=$(uname -m)   # x86_64 or aarch64
base=https://github.com/toon-protocol/toon_cli/releases/download/$version
curl -fsSLO "$base/toon-$version-linux-$arch.tar.gz"
curl -fsSLO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "toon-$version-linux-$arch.tar.gz"
install -D "toon-$version-linux-$arch/toon" ~/.local/bin/toon
toon --version
```

Or from source, with a Rust toolchain:

```sh
cargo install --locked --git https://github.com/toon-protocol/toon_cli --tag v0.1.0
```

Other versions are listed on the [releases page](https://github.com/toon-protocol/toon_cli/releases).

## Quick start

This runs on **devnet** (Base Sepolia), the default, so it costs no real money.

**1. Choose a wallet passphrase.** `toon` reads it from the environment and never
from a flag, because a flag shows up in the process list.

```sh
mkdir -p ~/.config/toon
printf '%s\n' 'your passphrase' > ~/.config/toon/passphrase
chmod 600 ~/.config/toon/passphrase
export TOON_PASSPHRASE_FILE=~/.config/toon/passphrase
```

**2. Create the wallet and the first TOON app.** The flag agrees to the
Anyone Protocol's terms, which running a hidden service
requires. Use `--clearnet <hostname>` instead if you want a public hostname.

```sh
toon init --accept-anyone-terms
```

```text
Wallet created at ~/.toon/agent-node/keystore.json.

Your mnemonic. It is shown this once and no command shows it again; write it down now:

  …twelve words…

TOON app created: relay. Fund its settlement keys, then run `toon up` to start it.
relay is a hidden service. Its onion endpoint is waqdg….anyone: …

Before `toon up` can start the connector, fund:
  0x5B67…Ef13 (evm): 1 token (1000000 base units of 0x0C99…C0E1)
Run `toon wallet fund` to fund them from the devnet faucet.
```

**3. Fund the wallet.** The devnet faucet sends the token. It sends no ETH: if you plan
to join a network (step 5), send about 0.001 Base Sepolia ETH for gas to the EVM
address shown by `toon wallet show`, using any public Base Sepolia faucet.

```sh
toon wallet fund
```

**4. Start the node.** This installs and starts a `systemd --user` unit, so the node
keeps running after your session ends.

```sh
toon up
toon status
```

**5. Join the network.** This opens a payment channel toward the network's connector
and spends the deposit, so it needs `--yes`:

```sh
toon join devnet --deposit 1000000 --yes
```

`toon logs relay` shows the relay's log, and `toon down` stops everything.

## Commands

Run `toon <command> --help` for the details of any command.

| | |
| --- | --- |
| **Set up** | `init`, `wallet show \| fund \| balances \| backup \| restore`, `limit show \| set` |
| **Run** | `up`, `down`, `status`, `logs <name>` |
| **Connect** | `join <network>`, `peer`, `channel`, `route list \| add \| price \| remove` |
| **Grow** | `create` a second TOON app, `add` / `remove` an app behind a connector, `destroy` a TOON app |
| **Relay and Nostr** | `relay` (the relay and its write price), `event` (publish and read under the agent identity), `nip` (draft and publish a NIP) |
| **Agents** | `skill install` writes the skills shipped in the binary to the place an agent harness loads them from |
| **Payments** | `send <address> --amount <n> --yes` sends one paid packet |

The global options are `--json`, which prints exactly one JSON document, and
`--app <name>`, which picks the TOON app when you run more than one.

### Networks

`toon init --network <profile>` picks the chains:

| Profile | Chains | Money |
| --- | --- | --- |
| `devnet` (default) | Base Sepolia, Solana devnet | Test tokens from the devnet faucet |
| `sandbox` | The local [`infra`](https://github.com/toon-protocol/infra) sandbox's anvil and Solana validator | None |
| `mainnet` | Base, Solana mainnet-beta | **Real** |

The EVM side settles by default. `--solana` adds Solana. You can override any of the
profile's chain settings with `--evm-rpc-url`, `--evm-token`, and the other `--evm-*`
flags.

### Spending and safety

- **Spending limit.** The wallet pays out no more than a limit per command and per
  day. `toon limit show` shows it, and `toon limit set`, which needs the passphrase,
  changes it.
- **Hidden by default.** If the `anon` daemon can't start, `toon` fails. It never falls
  back to clearnet without being asked ([ADR 0003](docs/adr/0003-hidden-service-is-the-default-and-fails-closed.md)).
  A hidden service hides where the node is, not who it pays: payments are on a public
  chain.
- **Backups.** The mnemonic is shown once. `toon wallet backup` seals the keystore and
  every onion address key into one file. Restoring from the mnemonic alone gives you
  new onion addresses.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | The command did what was asked |
| 1 | The command failed; the error's code says why |
| 2 | The command line was not understood |
| 3 | There is no agent node on this machine |

`toon status` exits with the code that describes the node's state, so a check script
can use it directly. Every error code is listed in [`docs/exit-codes.md`](docs/exit-codes.md).

## How it works

`toon` is also the connector ([ADR 0001](docs/adr/0001-the-cli-embeds-the-connector-crates.md)).
It is built against the connector's crates at one pinned revision and a pinned relay
image. `toon --version` reports both.

`toon up` writes `~/.config/systemd/user/toon-agent-node.service`, which runs
`toon up --foreground`, the **supervisor**. The supervisor runs:

- the `anon` daemon, one per machine, for the hidden services
- the relay, from the pinned image, as a Docker container
- each connector, as a child process of the same binary. It restarts a connector that
  exits, and `toon status` shows how often.

If the relay stops, the supervisor stops with it and exits 1.

The design decisions are in [`docs/adr/`](docs/adr/). The protocol that relays and
agents share, where no existing NIP covers it, is drafted in [`nips/`](nips/).

## Development

```sh
cargo build
./target/debug/toon --help
```

The CI gate is the `gate` job in `.github/workflows/ci.yml`:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
```

The tests run the built binary against a temporary home, using stand-ins for the
chain, the overlay, the relay container and other operators' relays. Setting
`TOON_APP_COMMAND=<program>` runs each app as a local process instead of a container,
and the tests use it with `examples/fake_relay.rs`.
[`docs/end-to-end.md`](docs/end-to-end.md) is the manual run against the real ones.

Pushing a tag `v<version>`, where the version matches `Cargo.toml`, builds the binaries
and publishes a release (`.github/workflows/release.yml`).

## Support

Report bugs and ask questions in
[GitHub Issues](https://github.com/toon-protocol/toon_cli/issues).

## License

MIT
