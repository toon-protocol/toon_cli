# The mainnet profile defaults to one operator's node

The mainnet profile names a connector, `https://connector.mainnet.toonprotocol.dev/ilp`, and a relay, `wss://relay.mainnet.toonprotocol.dev`. They are one operator's node, run by Drew (`drew-dot-com`): ILP address `g.drew`, relay at `g.drew.relay`. The maintainer accepted it as the default in issue #174. The names are stable ones under `toonprotocol.dev`, not the `sslip.io` names bound to the node's IP, because the default is compiled into the binary.

We chose this over leaving mainnet with no default, which makes every operator find and type two URLs. `--connector-url` and `--relay-url` at `init` still override both.

## Consequences

- It is one operator's node on one machine. If it is down, a new mainnet agent node has nothing to join by default, and there is no second mainnet connector to name beside it.
- It is a trust decision made for everyone downstream. The default is visible in what `init` records and what `toon status` shows, and is overridable at `init`.
- The names are under `toonprotocol.dev`, so the project can repoint them without a new release.
- The node settles on Base mainnet USDC and Solana mainnet-beta USDC. No facilitator is named on EVM, so a Base payer deposits from its own ETH; Solana opens are sponsored by the node.
- The default relay is the TypeScript relay, not the Rust relay this CLI runs, and the node's connector release trails the revision this CLI embeds.
- The default connector dialling an agent node's onion address is untested.
