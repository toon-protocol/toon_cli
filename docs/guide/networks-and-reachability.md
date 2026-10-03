# Networks and reachability

Two choices are made at `toon init`: which **network** the agent node settles on and joins,
and how its connectors are **reached**.

## Networks

```sh
toon init --network devnet  --accept-anyone-terms    # the default
toon init --network sandbox --accept-anyone-terms --connector-url … --relay-url …
toon init --network mainnet --accept-anyone-terms
```

| Network | Chain | Faucet | Network to join |
| --- | --- | --- | --- |
| `devnet` | Base Sepolia, a test token | `toon wallet fund` sends the token, no ETH | The devnet's hub and relay |
| `sandbox` | The local chain of the `infra` sandbox | None: fund from the sandbox's tooling | Its hub, which you name |
| `mainnet` | Real funds | None | None yet: name one with `--connector-url` and `--relay-url` |

`toon join <network>` only joins the network `init` was given, and is refused with
`join_refused`, spending nothing, when the profile names no connector.

An agent node initialised with `--solana` settles on two chains, as may the network's hub. Then
`toon join <network> --deposit <n> --yes` is refused (`peer_failed`, nothing spent) until you
name one: add `--chain evm` or `--chain solana`. No chain is chosen for you.

**Sandbox.** It allows plaintext `http://` peers by itself. A hidden agent node cannot reach
`localhost`, so name the hub at its onion endpoint:

```sh
toon init --network sandbox --accept-anyone-terms \
  --connector-url http://<hub>.anyone:3200/ilp --relay-url ws://<hub>.anyone:7100
```

[`docs/end-to-end.md`](../end-to-end.md) runs a whole agent node against the sandbox.

**Overrides.** `--evm-rpc-url`, `--solana-rpc-url`, `--evm-token`, `--faucet-url`, `--connector-url` and
`--relay-url` replace one setting of the profile; `toon init --help` lists the rest.
`--solana` settles on Solana too, and `--solana-rpc-url` replaces its JSON-RPC endpoint.

## Hidden service

The default. Each connector:

- listens on loopback only, and is reached at its own **onion endpoint**, a `.anyone`
  address on the Anyone overlay, on port 80. The relay is read at the same address on port
  7100.
- sends everything it sends through the overlay, chain RPC included. So do the requests
  `toon` itself makes on a hidden agent node: the faucet, the chain, other relays. Only a
  plain `http://` or `ws://` endpoint on this machine is dialled directly.
- keeps its address across restarts. The address key is derived by the wallet and sealed
  into a [backup](backup-and-restore.md).

It **fails closed**. If the overlay cannot come up, `init` and `create` create nothing,
and a command that needs the overlay fails with `overlay_unavailable`. Nothing ever falls
back to clearnet, because that would publish where the machine is without your asking.

`toon` downloads the pinned `anon` release, checks its SHA-256 and runs one daemon per agent
node. `TOON_ANON_MIRROR` replaces where it is downloaded from; the checksum still applies.

A hidden service hides where a TOON app is reachable, not whom it pays: payments are on a
public chain.

## Clearnet

```sh
toon init --clearnet node.example.com --listen 127.0.0.1:8080
```

The connector binds `--listen` (a free loopback port if omitted) and is published as
`node.example.com`. The certificate and the reverse proxy that answer there and pass to the
listen address are yours to provide, for example with Caddy:

```
node.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

A clearnet TOON app needs no overlay and no `--accept-anyone-terms`. `toon create` takes the
same `--clearnet` and `--listen`, so one agent node can mix hidden and clearnet TOON apps.

## Relay URLs

A relay URL is always `ws://` or `wss://`. `wss://` verifies the certificate against the
roots compiled into `toon`. For a private deployment, `TOON_TRUSTED_ROOT` names a PEM file of
extra roots to trust; it never turns verification off.
