# A zk edge

A **zk edge** is a connector that also takes packets paid from **zk allowances** (connector ADR
0080). A payer deposits native MINA into a vault, then opens a zk allowance at your connector
against one of its notes. The proof shows the note is in the vault and unspent without saying
which note it is, so you can't link a zk allowance to a deposit. The payer then pays packets from
it as from a channel. `toon` turns a TOON app's connector into a zk edge and back. The payer's
side, which covers depositing, notes, opening zk allowances and leaving, is the wallet in
`toon-client`. ADR 0016 records why.

## Before you turn it on: what runs beside it

The connector checks proofs and keeps the books, but it moves no MINA. You have to run two programs
beside the agent node, and `toon` runs neither:

- **A collector**, which takes what the zk edge has earned out of the vault. Without one, your
  zk edge is **never paid**: it forwards packets for payers and pays the next hops in your
  settlement token, and nothing comes back.
- **A challenger** (`zk-toon-challenger`, from zk_toon), which watches the vault and defeats a
  payer's dishonest escape before its deadline. Without one, a payer can escape with money your
  zk edge has already forwarded for them, and **you lose it**.

Until the zk edge signs mutual-close clearances (connector#1556), its payers can leave the vault
**only by escape**, so the challenger isn't optional.

## Turn it on

```sh
toon zk enable \
  --vault B62q... --network mina:devnet \
  --graphql-url https://api.minascan.io/node/devnet/v1/graphql \
  --archive-url https://api.minascan.io/archive/devnet/v1/graphql \
  --tier 100000000 --tier 1000000000 \
  --open-budget-cap 10000000000 \
  --rate 0.40 \
  --yes
```

This

- writes a `[zk_note]` table into the connector config of the first TOON app, the relay's (`--app
  <name>` picks another TOON app), and **restarts that connector**, which drops the packets it holds in
  flight, so it needs `--yes`;
- keeps the settings in the agent node's state, so every `toon up` writes the table again;
- checks the changed config with the connector's own validation **before** anything restarts. A
  setting the connector refuses, such as a tier zk_toon doesn't publish, answers
  `zk_edge_invalid` with the connector's reason, and nothing changes.

| Flag | What it is |
| --- | --- |
| `--vault` | The vault's zkApp address, `B62…`: what payers deposit into |
| `--network` | `mina:devnet` or `mina:mainnet` |
| `--graphql-url` | A Mina daemon's GraphQL, where the vault's account is read |
| `--archive-url` | A Mina archive node's GraphQL, where the vault's actions are read |
| `--tier` | A zk allowance budget you offer, in nanomina; repeat it for more. Each must be one zk_toon publishes |
| `--open-budget-cap` | The most the budgets of all open zk allowances may add up to, in nanomina: what you risk at once |
| `--zk-allowance-lifetime` | The longest a zk allowance lives, in seconds; 3600 unless named, at most 43200 |
| `--subsidy-budget` | Nanomina per subsidy period for subsidised zk allowances; 0, the default, offers none |
| `--rate` | Whole settlement tokens per MINA, as a decimal (`0.40`) or a fraction (`2/5`) |

`toon` fills in the rest. `verification_key_hash` is the hash of the one request key this
connector build carries. The **state key**, which signs the zk edge's successor states, is derived
from the wallet, so the same wallet makes the same one: after a restore, enabling the zk edge
again on the same TOON app brings it back. On a hidden service the vault is read
through the overlay, as the chain is, unless an endpoint is plain HTTP on this machine. The
connector reads the vault before it serves anything, so a vault it can't read keeps it from
starting.

### The rate

A zk edge prices in MINA and pays its next hop in your settlement token. No pool quotes MINA, so
the connector converts at the rate you declare, and that rate holds until the next restart. It
doesn't follow the market. **Build your spread into it:** declare less than MINA is worth, by as
much as the market might move before you change it, because forwarding at a rate above MINA's
value pays out more than comes in. `--rate` is required when the TOON app settles on a chain,
since every packet it forwards crosses from MINA into that chain's token. `toon` writes one rate
for each settlement table, scaled to its token's decimals. A TOON app that settles on the
sandbox's Solana can't be a zk edge, because that cluster has no fixed genesis hash to name its
token by.

## Change it

Run `toon zk enable` again with the settings you want. It replaces them all, so name every
setting again, not just the one that changes. The connector restarts once.

## Turn it off

```sh
toon zk disable --yes
```

removes the table and restarts the connector. The vault and its notes are the payers', not the
connector's. Zk allowances still open can't be paid at your connector any more, and their payers
leave by escape, which your challenger still has to watch.
