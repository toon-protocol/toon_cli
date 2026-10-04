# Money: wallet, limits and channels

Amounts are always in the token's **base units**. On the devnet the token has 6 decimals,
so `1000000` is one token.

## The wallet

`toon init` generates one mnemonic and derives every key from it: a settlement key per
connector and chain, each connector's identity, the agent identity that signs events, and
the key that signs your spending limit.

```sh
toon wallet show        # addresses by chain, the identities (needs the passphrase)
toon wallet balances    # what each address holds, by TOON app and chain
```

## Funding

Each connector's settlement key needs two things:

| What | Needed for | If missing |
| --- | --- | --- |
| At least one whole token | `toon up` to start that connector | `toon up` fails with `unfunded` |
| A little ETH for gas | Anything that sends a transaction: `join`, `peer add` and `create` with a deposit, `channel open`, `fund`, `withdraw`, `land` | That command fails with `unfunded`, before anything is charged or sent |

The `unfunded` message names each address and the amount it needs. Where the funds come
from depends on the network:

- **devnet**: `toon wallet fund` asks the faucet for the token. The faucet sends no ETH; get
  Base Sepolia ETH from a public faucet.
- **sandbox** and **mainnet**: there is no faucet (`faucet_unavailable`). Send funds to the
  addresses `toon wallet show` lists.

With `--solana` at `init`, the Solana key also needs the token and 0.01 SOL before `up`.

## Commands that move money

| Command | What it moves |
| --- | --- |
| `toon send <address> --amount <n> --yes` | One packet of `n` |
| `toon probe <address> --amount <n> --yes` | Up to `n`; with no `--amount` a probe moves nothing and needs no `--yes` |
| `toon peer add <url> --deposit <n> --yes` | `n` into a new channel |
| `toon join <network> --deposit <n> --yes` | `n` into a channel toward the network's hub |
| `toon create <name> --deposit <n> --yes` | `n` into each of two channels: `2n` |
| `toon event publish --relay <url> --yes` | The relay's price, or `--amount` |
| `toon relay subscribe <url> --amount <n> --yes` | Up to `n`, in whole packets |

The three peerings take an optional `--chain evm|solana`. It is needed when your connector and
the other settle on more than one chain: the connector refuses to choose for you, the command
fails with `peer_failed` and says to run it again with `--chain`, and nothing is counted
against the spending limit. `create` applies its one `--chain` to both peerings.

Each states its amount and does nothing without `--yes`: it fails with `not_confirmed` and
prints what it would have moved. So the safe way to see what a command costs is to run it
without `--yes`:

```sh
$ toon send g.toon.3fa29c01b2d4e5f6.search --amount 5
error: This moves 5 base units. Add `--yes` to say that you mean it.
```

`toon channel open` and `toon channel fund` lock collateral but take no `--yes`; check
`toon limit show` and `toon wallet balances` before you run them.

## The spending limit

The wallet pays out at most `--max-per-command` in one command and `--max-per-day` in one
UTC day. A payment over either is refused with `spending_limit` before anything is sent.

```sh
toon limit show
# Per command: 10000000. Per day: 100000000. Remaining today: 100000000. ...

toon limit set --max-per-command 5000000 --max-per-day 50000000
```

The defaults are set at `toon init` and can be given there too. `limit set` needs the
wallet passphrase, and the limits are signed by a key the wallet derives. An agent that
can run commands but does not hold the passphrase cannot raise its own limit, and an edited
`limits.json` stops every payment until `limit set` signs it again.

### What a failed payment costs

A packet that is rejected may still have cost something: a connector farther along may have
been paid to forward it. `toon` measures what your outbound channels moved by across the
packet and reports it as `paid`, and counts that against the limit:

```sh
toon send g.toon.3fa29c01b2d4e5f6.search --amount 5 --yes --json | jq '{outcome, paid, reject}'
```

`paid` is `0` when your own connector rejected the packet before signing anything. A packet
that was never sent (the key could not be read, the packet could not be sealed) is not
counted either. Any other failure stays counted, because the packet may have left.

## Channels

Peerings open channels for you. These commands manage them directly:

```sh
toon channel list                       # both directions, with collateral, watermark, status
toon channel fund <id> --amount 500000  # add to an outbound channel
toon channel withdraw <id>              # start withdrawing; run again to finish
toon channel land <id>                  # land the latest voucher on an inbound channel now
```

The outbound **watermark** is how much you have signed away on a channel so far. Watching
it move is the most direct way to see what payments cost.

To open a channel toward a counterparty you are not peering with, save its
`batchSettlements` entry for one chain, as its self-description publishes it, to a file:

```sh
toon channel open --terms their-terms.json --deposit 1000000
```

`--app <name>` points any channel command at a TOON app other than the first.
