# Peering

A connector reaches other connectors only over **peerings**: standing arrangements to
forward packets to a connector, each paid on a channel it opens toward that connector.

Three things to hold on to:

1. **A peering goes one way.** Yours lets *you* send to them. They send to you only if
   their operator peers toward you as well.
2. **A peering is paid for up front.** It opens a channel with a deposit, so it needs
   `--deposit <n> --yes` and counts against the spending limit.
3. **A peering alone carries nothing.** A *route* says which ILP addresses go over it.
   `toon join` adds the route for you; `toon peer add` does not.

## Join a network

The simplest peering is toward the network's own connector, its hub. It reaches
everything on the network:

```sh
toon join devnet --deposit 1000000 --yes
toon peer list        # a peering labelled devnet
toon route list       # a route for g.toon over it
```

You can join only the network `init` was given, and only once. `join_refused` says which
rule stopped it; nothing was spent.

## Send a packet

Send a packet to any ILP address your routes reach. To an app behind a connector that is
not yours, add `--seal-to` with that connector's `/ilp` URL, so the request inside the
packet is sealed to it and only it can read it:

```sh
toon send g.toon.3fa29c01b2d4e5f6.search --amount 10 \
  --seal-to http://abc…xyz.anyone/ilp --yes --json
```

The report says `fulfilled`, with the app's answer, or `rejected`, with a reject code in
`reject.code`, and always `paid`. A rejected packet exits 1.

A connector on the way may charge to forward. It rejects a packet that does not carry its
price with `F03`; send the whole path's cost in `--amount`.

## Walkthrough: peer with another operator

Alice and Bob each run an agent node. Alice wants to publish to Bob's relay; later Bob
wants to pay Alice's apps.

**1. Bob shares his connector's address.** It is in his `status`:

```sh
# Bob
toon status --json | jq '.agent_node.toon_apps[0].connector.ilp_address'
# "g.toon.9c41e07d2a6b3f18"
```

His connector's URL is his onion endpoint, from `toon init`, with `/ilp`: for example
`http://bob…anyone/ilp`. He sends Alice both.

**2. Alice peers toward Bob, and routes his addresses over the peering.**

```sh
# Alice
toon peer add http://bob…anyone/ilp --deposit 1000000 --id bob --yes
toon route add g.toon.9c41e07d2a6b3f18 --peer bob
```

`--id` labels the peering; the label is what `route add --peer` and `peer remove` take.

**3. Alice pays Bob's relay.**

```sh
# Alice
toon event publish --kind 1 --content "hi Bob" --relay ws://bob…anyone:7100 --yes
```

Had she skipped step 2, the publish would fail with `peering_needed` and print the exact
`peer add` and `route add` to run. Nothing is paid and nothing is created for her.

**4. For Bob to pay Alice**, he repeats steps 1 and 2 in the other direction.

Running `peer add` again with the same URL finds the channel it already opened: it reports
`deposited: false` and spends nothing.

## Run a peering as a forwarder

When others send packets through your connector to a peer, you may keep a fee of each
packet you forward over that peering, and cap how much one packet may carry:

```sh
toon peer add http://carol…anyone/ilp --deposit 1000000 --id carol \
  --fee 100 --max-packet-amount 50000 --yes
toon route add g.toon.5d0e8f2a91b7c364 --peer carol
```

## Remove a peering or a route

```sh
toon route remove g.toon.9c41e07d2a6b3f18
toon peer remove bob
```

Collateral comes back only through the chain: find the channel in `toon channel list` and
withdraw it with `toon channel withdraw` ([Money](money.md#channels)).

## When it refuses

| Error code | Meaning |
| --- | --- |
| `peer_not_peerable` | The other connector refuses peerings. Only its operator can change that |
| `peer_failed` | Your connector refused the peering, or could not be reached |
| `route_failed` | A route write was refused, or the prefix is not usable |
| `peering_needed` | No peering reaches the relay you tried to pay; the message names what to run |
| `unfunded` | The settlement key lacks the gas for the deposit; nothing was sent |
