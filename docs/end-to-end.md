# The end-to-end run

The gate substitutes four things: the chain, the overlay, the app runner and the remote
relay. This run uses the real ones: the `anon` daemon on the Anyone network, the relay
image in a container, the `infra` sandbox's local chain and hub, and, once one sells a
feed, a relay to subscribe to. "Hub" and "relay node" below are the sandbox's own names
for its connector and for a connector with a relay behind it. It is run by hand,
before a release and after moving the connector pin, the relay image or the `anon`
release. It is not part of the gate, because it needs Docker and a network nobody here
controls.

Record every run as a comment on the spec (#2): the date, `toon --version`, and for each
step below whether it did what the step says. Open a ticket for every step that did not.

## Steps known to fail

Take a row out when its ticket closes, and the flags or the note beside it out of the
step.

| Step | What happens today | Ticket |
| --- | --- | --- |
| 2, `init` | The sandbox profile has the wrong token and connector, hence the three flags | #67 |
| 2, hold a subscription | No relay serves the subscribe route | relay #215 |
| 3, `join` | `unfunded` asks for 0.0001 ETH, and the deposit cost 0.0004; with too little the `join` fails with `peer_failed`, "out of gas" | #101 |
| 3, `join` | The deposit lands and the `join` fails with `peer_failed`, "the chain shows no balance there"; the same command again finds the channel, and each attempt is counted against the day's spending | #102 |

## What it needs

- Docker, `jq`, `curl`, and Foundry's `cast`.
- The `infra` checkout beside this one, set up once with `make setup` in `infra/sandbox`.
- For the last step of part 3, about 0.001 Base Sepolia ETH from a public faucet.

The run never touches your own agent node: every command below runs with `HOME` set to a
directory made for the run.

```sh
cargo build --locked
export E=/tmp/toon-e2e && mkdir -p $E/sandbox $E/other $E/devnet
cp target/debug/toon $E/toon          # a rebuild must not replace a running supervisor
printf 'a passphrase for this run\n' > $E/pass && chmod 600 $E/pass
export TOON_PASSPHRASE_FILE=$E/pass
docker build -t toon-e2e-app docs/end-to-end/app
```

`toon-e2e-app` is the app the run puts behind a connector: it answers every request with
200 on port 3100, which is what `toon` asks of an app's image.

## 1. The sandbox, with its hub hidden

```sh
make -C ../infra/sandbox clean
make -C ../infra/sandbox up-topology NODES="relay relay2" CHAINS=evm HS=relay
```

`clean` first, because the sandbox's chain keeps nothing and its connectors do: started on
the volumes of an earlier run, the peerings fail with `batch-settlement channel … not
found`. `up-topology` dials the real Anyone network and takes a minute or two. Its last
lines name the hub: `relay is reached at http://<hub>.anyone:3200/ilp`. The same address
serves the hub's relay on port 7100. A second relay node, `g.toon.relay2`, sits behind the
hub, with its connector at `http://localhost:3290/ilp` and its relay at
`ws://localhost:7110`. The sandbox's own client daemon is a SOCKS proxy at
`127.0.0.1:19050`: the "other machine" a hidden service is checked from.

A relay node of the sandbox that is not hidden is paid through the hub and is not peered
with directly: it publishes a compose-network name, and a connector dials what the other
side publishes.

```sh
export HUB=<hub>.anyone
export USDC=0x0A867CA0442383c2A89951244B955AA19b615b58   # the sandbox's FiatToken
export RPC=http://localhost:8545
```

## 2. A hidden agent node against the sandbox

```sh
export HOME=$E/sandbox
$E/toon init --network sandbox --accept-anyone-terms --allow-plaintext-peers \
  --evm-token $USDC --connector-url http://$HUB:3200/ilp --relay-url ws://$HUB:7100 \
  --json > $E/init.json
jq '.toon_apps[0].onion_endpoint, .wallet.chains.evm[0].address' $E/init.json
```

**Expect** `created: true` and an onion endpoint ending in `.anyone`. `init` downloads the
pinned `anon` release, checks it and bootstraps it; if the Anyone network does not carry,
it fails with `overlay_unavailable` and creates nothing. The chain RPC is on this machine
over plain http, which is the one RPC a hidden connector dials directly.

The sandbox has no faucet. Fund the address from anvil's first account and the token's
minter, both committed throwaway keys of the sandbox:

```sh
fund() {
  cast send $1 --value 1ether --rpc-url $RPC \
    --private-key 0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80
  cast send $USDC 'mint(address,uint256)' $1 $2 --rpc-url $RPC \
    --private-key 0xc511b2aa70776d4ff1d376e8537903dae36896132c90b91d52c1dfbae267cd8b
}
fund $(jq -r '.wallet.chains.evm[0].address' $E/init.json) 100000000
$E/toon wallet balances --json
```

Start the agent node in a second terminal, with the same `HOME` and
`TOON_PASSPHRASE_FILE`, and leave it running:

```sh
$E/toon up --foreground --json
```

**Expect** one JSON document naming the connector's address, a `toon-<id>-relay` container
from the pinned image in `docker ps`, and `$E/toon status --json` exiting 0 with the
connector and the relay running. The relay's ILP address is under the connector's address
segment: `g.toon.<segment>.relay`.

### It is reachable at its onion endpoint

```sh
ME=$(jq -r '.toon_apps[0].onion_endpoint' $E/init.json)
curl -s --socks5-hostname 127.0.0.1:19050 http://$ME/ilp | jq '{ilpAddresses, httpEndpoint}'
curl -s --socks5-hostname 127.0.0.1:19050 -H 'Accept: application/nostr+json' http://$ME:7100/ | jq .
```

**Expect** the connector's self-description, naming the onion endpoint as its
`httpEndpoint`, and the relay's information document on port 7100 of the same address,
whose `toon` object names the relay's ILP address, the connector's onion endpoint, its
seal key and a price of 1. Both go through a daemon that is not the agent node's own.

### Join

```sh
$E/toon join sandbox --deposit 5000000 --yes --json
$E/toon channel list --json
$E/toon send g.toon.relay2 --amount 101 --seal-to http://localhost:3290/ilp --yes --json
$E/toon send g.toon.relay --amount 1 --seal-to http://$HUB:3200/ilp --yes --json
$E/toon channel list --json
```

**Expect** a peering `sandbox` with a channel of 5000000, opened by the connector dialling
the hub at its `.anyone` address through the agent node's daemon. The packet to
`g.toon.relay2` is forwarded by the hub, which keeps 100 of the 101, and is `fulfilled`;
it carries no event, so the relay behind it answers 400. The second packet is sealed to
the hub's own identity, which the command line reads at the hub's `.anyone` address
through the agent node's daemon; it is `fulfilled` at the hub's price of 1. The channel's
watermark is then 102.

### Publish an event

```sh
$E/toon event publish --kind 1 --content "end-to-end run" --json > $E/event.json
READ=$($E/toon status --json \
  | jq -r '.agent_node.toon_apps[0].apps[] | select(.name == "relay") | .read_address')
$E/toon event query ws://$READ --filter "{\"ids\":[\"$(jq -r .event.id $E/event.json)\"]}" --json
```

**Expect** `outcome: published` in `event.json`, with the relay's own answer naming the
event id, and the query returning that event. The write costs the operator nothing
although the route's price is 1.

### Publish to another relay

The hub's relay, at its `.anyone` address, then `relay2`, which is reached through the
hub:

```sh
$E/toon event publish --kind 1 --content "to the hub's relay" \
  --relay ws://$HUB:7100 --yes --json > $E/hub-event.json
$E/toon event query ws://$HUB:7100 \
  --filter "{\"ids\":[\"$(jq -r .event.id $E/hub-event.json)\"]}" --json
$E/toon event publish --kind 1 --content "to relay2" \
  --relay ws://localhost:7110 --amount 101 --yes --json > $E/relay2-event.json
$E/toon event query ws://localhost:7110 \
  --filter "{\"ids\":[\"$(jq -r .event.id $E/relay2-event.json)\"]}" --json
$E/toon channel list --json
```

**Expect** the first `published` with `paid: 1` and the query returning the event: the
command line read the hub's information document and the event through the agent node's
daemon, and sealed the write to the key the document pins. The watermark is then 103.
The second is `published` with `paid: 101` and read back from `relay2`: its price is 1,
the hub keeps 100 to forward the write, and `--amount` states the two together. The
watermark is then 204. Without `--amount` the write is sent for the relay's price and the
hub rejects it with `F03`; the report says `paid: 1`, and the watermark and the day's
spending have each moved by that 1.

### Create a second TOON app

```sh
$E/toon create second --image toon-e2e-app --deposit 1000000 --accept-anyone-terms --yes --json \
  > $E/create.json
```

The first attempt fails with `unfunded` and names the new TOON app's settlement address.
Fund it with `fund <address> 10000000` and run the command again.

**Expect** a second onion endpoint, different from the first, an ILP address under an
address segment of its own (`g.toon.<segment>`), and two peerings, each with a channel of
1000000. Then pay the app behind the new connector from the first one:

```sh
SECOND=$(jq -r .created.listen $E/create.json)
APP=$(jq -r .created.address $E/create.json).second
$E/toon send $APP --amount 1 --seal-to http://$SECOND/ilp --yes --json
$E/toon --app second channel list --json
```

**Expect** `fulfilled` with the app's answer, and the inbound channel of `second` at
watermark 1. The packet crosses from one connector to the other at the second's onion
endpoint; `--seal-to` is only where the command line reads the key the payload is sealed
to, and on this machine the second connector's loopback address answers that.

### Add an app

```sh
$E/toon add notes --to second --image toon-e2e-app --price 2 --yes --json > $E/add.json
$E/toon send $(jq -r .address $E/add.json) --amount 2 --seal-to http://$SECOND/ilp --yes --json
$E/toon status --json
RELAY_IMAGE=$($E/toon --version | sed 's/.*relay \([^@)]*\).*/\1/')
$E/toon add archive --to second --image $RELAY_IMAGE --yes --json
```

**Expect** `restarted: true` in `add.json`, the packet `fulfilled` over the route `create`
made (a first packet sent while the restarted connector is still coming back is rejected
with `T01`, which is expected: its report says `paid: 2`, what the rejected packet cost,
so send it again), and status showing `second` with two apps, both running. The last
command is refused with `one_relay`: an agent node runs one relay.

### Another agent node publishes to this one's relay

A second hidden agent node, in a home of its own, peers with the first at its onion
endpoint and pays its relay:

```sh
export HOME=$E/other
$E/toon init --network sandbox --accept-anyone-terms --allow-plaintext-peers \
  --evm-token $USDC --connector-url http://$HUB:3200/ilp --relay-url ws://$HUB:7100 \
  --json > $E/other-init.json
fund $(jq -r '.wallet.chains.evm[0].address' $E/other-init.json) 100000000
```

Start it in a third terminal, with this `HOME` and `TOON_PASSPHRASE_FILE`, as the first
one was started:

```sh
$E/toon up --foreground --json
```

Then, in the first terminal:

```sh
$E/toon peer add http://$ME/ilp --deposit 1000000 --id first --yes --json
$E/toon route add $(HOME=$E/sandbox $E/toon status --json \
  | jq -r '.agent_node.toon_apps[0].connector.ilp_address') --peer first --json
$E/toon event publish --kind 1 --content "from another agent node" \
  --relay ws://$ME:7100 --yes --json > $E/other-event.json
$E/toon event query ws://$ME:7100 \
  --filter "{\"ids\":[\"$(jq -r .event.id $E/other-event.json)\"]}" --json
$E/toon down --json
export HOME=$E/sandbox
$E/toon channel list --json
```

**Expect** `published` with `paid: 1`, the event read back from the first agent node's
relay at its onion endpoint, and the first agent node's inbound channel from the other
one at watermark 1. Everything between the two crosses the overlay, from one hidden
service to another. Before the `route add`, the publish fails with `peering_needed`, and
its message names the `peer add`, with `--deposit <amount> --yes`, and the `route add` to
run.

### Hold a subscription

```sh
$E/toon relay subscribe ws://$HUB:7100 --filter '{"kinds":[1]}' --amount 10 --yes --json
$E/toon relay subscribe ws://localhost:7110 --filter '{"kinds":[1]}' \
  --amount 1010 --packet-amount 101 --yes --json
$E/toon relay subscriptions --json
```

**Expect** a balance at each relay, the hub's and `relay2`'s, and their events arriving
in the agent node's own relay. A packet to `relay2` is forwarded by the hub, so
`--packet-amount` is the hub's price for that route, 101, as `--amount` was for the
write: the hub keeps 100 of each packet, and ten packets credit ten times `relay2`'s
subscribe price. While no relay sells a feed each command fails with `relay_not_payable`,
having read the relay's information document, the hub's through the overlay, and the step
is recorded as not run.

### Stop

```sh
$E/toon destroy second --json    # refused with funds_held: its channels hold funds
$E/toon down --json
```

**Expect** `down` to stop the supervisor in the other terminal, both connectors, the
containers and the `anon` daemon.

## 3. A hidden agent node and a clearnet network

This part settles whether a hidden agent node reaches a network that is on clearnet, the
devnet, through the overlay.

```sh
export HOME=$E/devnet
$E/toon init --network devnet --accept-anyone-terms --json > $E/devnet-init.json
$E/toon wallet fund --json
$E/toon wallet balances --json
```

**Expect** the faucet to send the token and no ETH, and the balance to show it: the
command line asked the faucet and the chain through the agent node's daemon. `init`
stopped the daemon when it was done, so start it again from the files `init` left, and
ask for each thing the agent node needs through its proxy:

```sh
O=$HOME/.toon/agent-node/overlay
rm -f $O/anon.log                     # it still says the daemon of `init` bootstrapped
$O/bin/*/anon -f $O/anonrc & ANON=$!
until grep -q 'Bootstrapped 100%' $O/anon.log; do sleep 2; done
P=$(sed -n 's/^SocksPort //p' $O/anonrc)
via() { curl -s -m 60 --socks5-hostname $P -o /dev/null -w "%{http_code} $1\n" "$@"; }
via https://base-sepolia-rpc.publicnode.com -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'
via https://faucet.devnet.toonprotocol.dev/
via "$(jq -r .connector_url $HOME/.toon/agent-node/state.json)"
via "$(jq -r .relay_url $HOME/.toon/agent-node/state.json | sed 's/^ws/http/')" \
  -H 'Accept: application/nostr+json'
kill $ANON
```

**Expect** 200 from all four. Then the agent node itself, with no ETH yet. Start it in
a second terminal:

```sh
$E/toon up --foreground --json
```

```sh
grep -E 'socks_proxy|listening' $HOME/.toon/agent-node/connectors/0/connector.log
$E/toon event query "$(jq -r .relay_url $HOME/.toon/agent-node/state.json)" \
  --filter '{"kinds":[1],"limit":2}' --json
$E/toon join devnet --deposit 1000000 --yes --json
```

**Expect** `settlement rpc via socks_proxy` and `connector listening`: the connector read
the chain through the overlay. The query returns events from the devnet's relay, read
over `wss://` through the overlay. The `join` is refused with `unfunded`, naming the
settlement address and the ETH it needs for gas, and the day's spending is unchanged.

The devnet faucet sends no ETH, and a connector pays the gas of its own deposit. Send
about 0.001 Base Sepolia ETH to that address from a public faucet (the 0.0001 the message
names is too little, #101), and join:

```sh
$E/toon join devnet --deposit 1000000 --yes --json
$E/toon channel list --json
$E/toon peer list --json
RELAY=$(jq -r .relay_url $HOME/.toon/agent-node/state.json)
$E/toon event publish --kind 1 --content "end-to-end run" --relay $RELAY --yes --json \
  > $E/devnet-event.json
$E/toon event query $RELAY \
  --filter "{\"ids\":[\"$(jq -r .event.id $E/devnet-event.json)\"]}" --json
$E/toon down --json
```

**Expect** a peering `devnet` with an open channel of 1000000, then `published` with
`paid: 1` and the event read back from the devnet's relay: a hidden agent node paid a
network on clearnet. If the `join` fails with "confirmed, and the chain shows no balance
there", the deposit landed and the read after it did not see it: run the `join` again,
and it reports the channel as `found` (#102).

## Afterwards

```sh
make -C ../infra/sandbox clean
rm -rf $E
```
