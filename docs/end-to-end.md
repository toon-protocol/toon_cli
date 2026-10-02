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

The run of 2026-10-01 is recorded on #2. Take a row out when its ticket closes, and the
flags or the note beside it out of the step.

| Step | What happens today | Ticket |
| --- | --- | --- |
| 2, `init` | The sandbox profile has the wrong token and connector, hence the three flags | #67 |
| 2, a home with a long path | `up` fails with `io`, naming `SUN_LEN` | #75 |
| 2, anything naming `$HUB` but `init` | The command line cannot reach a `.anyone` host, hence `localhost` in `--seal-to` | #69 |
| 2, hold a subscription | No relay serves the subscribe route | relay #215 |
| 3, the connector and the relay | The devnet profile's two hosts do not exist: both answer 000 | #66 |
| 3, `up` and `join` | The faucet sends no ETH and `up` requires it | #68 |

## What it needs

- Docker, `jq`, `curl`, and Foundry's `cast`.
- The `infra` checkout beside this one, set up once with `make setup` in `infra/sandbox`.
- A home directory with a short path: the supervisor's socket is under it, and a socket
  path is at most 107 bytes.

The run never touches your own agent node: every command below runs with `HOME` set to a
directory made for the run.

```sh
cargo build --locked
export E=/tmp/toon-e2e && mkdir -p $E/sandbox $E/devnet
cp target/debug/toon $E/toon          # a rebuild must not replace a running supervisor
printf 'a passphrase for this run\n' > $E/pass && chmod 600 $E/pass
export TOON_PASSPHRASE_FILE=$E/pass
docker build -t toon-e2e-app docs/end-to-end/app
```

`toon-e2e-app` is the app the run puts behind a connector: it answers every request with
200 on port 3100, which is what `toon` asks of an app's image.

## 1. The sandbox, with its hub hidden

```sh
make -C ../infra/sandbox up-topology NODES="relay relay2" CHAINS=evm HS=relay
```

It dials the real Anyone network and takes a minute or two. Its last lines name the hub:
`relay is reached at http://<hub>.anyone:3200/ilp`. The same address serves the hub's
relay on port 7100. A second relay node, `g.toon.relay2`, sits behind the hub, and the
sandbox's own client daemon is a SOCKS proxy at `127.0.0.1:19050`: the "other machine" a
hidden service is checked from.

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
from the pinned image in `docker ps`, and `toon status --json` exiting 0 with the connector
and the relay running.

### It is reachable only at its onion endpoint

```sh
ME=$(jq -r '.toon_apps[0].onion_endpoint' $E/init.json)
curl -s --socks5-hostname 127.0.0.1:19050 http://$ME/ilp | jq '{ilpAddresses, httpEndpoint}'
curl -s --socks5-hostname 127.0.0.1:19050 -H 'Accept: application/nostr+json' http://$ME:7100/ | jq .
```

**Expect** the connector's self-description, naming the onion endpoint as its
`httpEndpoint`, and the relay's information document on port 7100 of the same address.
Both go through a daemon that is not the agent node's own.

### Join

```sh
$E/toon join sandbox --deposit 5000000 --yes --json
$E/toon channel list --json
$E/toon send g.toon.relay2 --amount 101 --seal-to http://localhost:3290/ilp --yes --json
$E/toon channel list --json
```

**Expect** a peering `sandbox` with a channel of 5000000, opened by the connector dialling
the hub at its `.anyone` address through the agent node's daemon. The packet to
`g.toon.relay2` is forwarded by the hub, which keeps 100 of the 101, and is `fulfilled`;
it carries no event, so the relay behind it answers 400. The channel's watermark is then
101.

### Publish an event

```sh
$E/toon event publish --kind 1 --content "end-to-end run" --json | tee $E/event.json
READ=$(docker ps --format '{{.Names}} {{.Ports}}' | grep -- '-relay ' \
  | grep -o '127.0.0.1:[0-9]*->7100' | cut -d- -f1)
$E/toon event query ws://$READ --filter "{\"ids\":[\"$(jq -r .event.id $E/event.json)\"]}" --json
```

**Expect** `outcome: published`, with the relay's own answer naming the event id, and the
query returning that event. The write costs the operator nothing although the route's
price is 1.

### Create a second TOON app

```sh
$E/toon create second --image toon-e2e-app --deposit 1000000 --accept-anyone-terms --yes --json \
  | tee $E/create.json
```

The first attempt fails with `unfunded` and names the new TOON app's settlement address.
Fund it with `fund <address> 10000000` and run the command again.

**Expect** a second onion endpoint, different from the first, and two peerings, each with
a channel of 1000000. Then pay the app behind the new connector from the first one:

```sh
SECOND=$(jq -r .created.listen $E/create.json)
$E/toon send g.toon.second --amount 1 --seal-to http://$SECOND/ilp --yes --json
$E/toon --app second channel list --json
```

**Expect** `fulfilled` with the app's answer, and the inbound channel of `second` at
watermark 1. The packet crosses from one connector to the other at the second's onion
endpoint.

### Add an app

```sh
$E/toon add notes --to second --image toon-e2e-app --price 2 --yes --json
$E/toon route add g.toon.notes --peer second --json
$E/toon send g.toon.notes --amount 2 --seal-to http://$SECOND/ilp --yes --json
$E/toon status --json
```

**Expect** `restarted: true`, the packet `fulfilled` (a first packet sent while the
restarted connector is still coming back is rejected with `T01`: send it again), and
status showing `second` with two apps, both running.

### Hold a subscription

```sh
$E/toon relay subscribe ws://localhost:7100 --filter '{"kinds":[1]}' --amount 10 --yes --json
$E/toon relay subscriptions --json
```

**Expect** a balance at that relay, and its events arriving in the agent node's own
relay. While no relay sells a feed the command fails with `relay_not_payable`, and the
step is recorded as not run.

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
```

**Expect** the faucet to send the token. `init` stopped the daemon when it was done, so
start it again from the files `init` left, and ask for each thing the agent node needs
through its proxy:

```sh
O=$HOME/.toon/agent-node/overlay
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
```

**Expect** 200 from all four. While the profile names the wrong hosts (#66), ask for the
devnet's own instead, which is what settles the question:

```sh
via https://proxy.relay.devnet.toonprotocol.dev/ilp
via https://relay-ws.devnet.toonprotocol.dev/ -H 'Accept: application/nostr+json'
```

Then the connector itself, whose settlement RPC goes
through the same proxy:

```sh
C=$HOME/.toon/agent-node/connectors/0
sleep 30 | $E/toon connector $C/connector.toml 2>&1 | grep -E 'socks_proxy|listening'
kill $ANON
```

**Expect** `settlement rpc via socks_proxy` and `connector listening`: the connector read
the chain through the overlay. Then the agent node as an operator runs it:

```sh
$E/toon up --foreground --json        # in a second terminal
$E/toon join devnet --deposit 1000000 --yes --json
$E/toon down --json
```

**Expect** a peering `devnet` with an open channel.

## Afterwards

```sh
make -C ../infra/sandbox down
rm -rf $E
```
