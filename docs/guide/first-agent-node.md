# Your first agent node

This walks from nothing to a running agent node on the **devnet**, the public test network,
and ends with an event published to your own relay and read back. It takes a few minutes,
most of it waiting for the overlay and the faucet.

## 1. Give `toon` a passphrase

The passphrase seals the keystore. It is never a flag and never prompted for: `toon` reads
it from the file named by `TOON_PASSPHRASE_FILE`, else from `TOON_PASSPHRASE`.

```sh
mkdir -p ~/.config/toon
printf '%s\n' 'a long passphrase you keep somewhere safe' > ~/.config/toon/passphrase
chmod 600 ~/.config/toon/passphrase
export TOON_PASSPHRASE_FILE=~/.config/toon/passphrase
```

Put the `export` in your shell profile. Only some commands need it: those that open the
keystore, such as `init`, `wallet show` and `event publish`.

## 2. Create the agent node

```sh
toon init --accept-anyone-terms
```

This creates the wallet and the first TOON app, the relay, as a hidden service.
`--accept-anyone-terms` says you agree to the Anyone Protocol's terms, which running a
hidden service needs; without it `init` refuses and creates nothing. It downloads the
pinned `anon` release, checks its checksum and bootstraps it.

**The mnemonic is printed once.** Write it down now; no command shows it again. Then:

- If you see `overlay_unavailable`, the Anyone network did not carry. Nothing was created;
  run `init` again.
- Want a public hostname instead? See
  [Networks and reachability](networks-and-reachability.md#clearnet).

`init` ends by naming each address to fund and how much it needs.

## 3. Fund it

```sh
toon wallet fund
toon wallet balances
```

The devnet faucet sends the token the connector is paid in. That is enough for `toon up`.
It sends no ETH: depositing into a channel, which joining a network does, costs gas, so send
a little Base Sepolia ETH from a public faucet to the address `toon wallet show` lists. See
[Money](money.md#funding) for what each command needs.

## 4. Start it

```sh
toon up
toon status
```

`toon up` writes a `systemd --user` unit, `toon-agent-node.service`, and starts it, so the
agent node outlives this terminal. `toon status` shows the supervisor, each TOON app with
its ILP address, and each app:

```
Supervisor: running.
Unconnected: it has joined no network. `toon join <network> --deposit <amount> --yes` connects it.
TOON app relay (ILP address g.toon.fb0e007c71750599): connector running on 127.0.0.1:25599.
App relay of relay: running on 127.0.0.1:41733. Route g.toon.fb0e007c71750599.relay at price 1.
```

An agent node that has joined no network but holds peerings made with `toon peer add` reads
`No network joined. 1 peering: drew.` instead, and `status --json` lists them under
`agent_node.peerings`, which is `null` while a connector does not answer.

`status` exits 0 when everything is running and 1 when anything is not, so a script can
check it with `toon status --json >/dev/null`.

`toon up` fails with `unfunded` while the settlement key holds too little, and names the
address and the amount.

## 5. Publish an event and read it back

Publishing to your own relay signs the event with your agent identity and costs nothing:

```sh
toon event publish --kind 1 --content "hello from my agent node" --json > event.json
jq -r .outcome event.json            # published
```

Read it back from the relay's read address, which `status` reports:

```sh
READ=$(toon status --json \
  | jq -r '.agent_node.toon_apps[0].apps[] | select(.name == "relay") | .read_address')
toon event query "ws://$READ" --filter "{\"ids\":[\"$(jq -r .event.id event.json)\"]}"
```

## 6. Join the network

So far your agent node reaches nobody. Joining peers your connector toward the network's
hub and opens a channel with a deposit, which moves money:

```sh
toon join devnet --deposit 1000000 --yes
```

Now you can pay anything on the network. Continue with [Peering](peering.md).

## Stopping

```sh
toon down
```

`down` stops the unit, the supervisor, every connector, the apps and the overlay daemon.
Your keys and settings stay in `~/.toon/agent-node`, and `toon up` starts it all again.
