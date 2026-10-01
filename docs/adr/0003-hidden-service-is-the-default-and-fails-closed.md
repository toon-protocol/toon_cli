# Hidden service is the default and fails closed

A new TOON app is a hidden service unless the operator explicitly asks for clearnet. Everywhere else in the fleet it is the reverse: the devnet is clearnet and the hidden service is an opt-in overlay (`provider`, the sandbox's `hs` profile). We made it the default because an agent node is run by an agent on whatever machine it has, usually with no public hostname, no certificate and no reason to reveal where it is.

If the `anon` daemon cannot bootstrap, creation fails. It never falls back to clearnet, because a fallback would publish the machine's location without the operator having asked.

## Consequences

- The supervisor downloads a checksum-pinned `anon` binary and runs it, one daemon per machine. The connector still never controls the daemon (connector ADR 0070); the CLI reads the generated address and renders it into the connector's config.
- Each connector gets its own address. The address key is part of the wallet's backup, since losing it changes the address every peer has bound.
- All outbound traffic goes through the overlay by default, settlement RPC included. A hidden listener with clearnet egress is not hidden (TOON Network ADR 0008).
- Anyone only (`.anyone`), not Tor. The operator agrees to Anyone's terms once, with an explicit flag at setup.
- It hides where a TOON app is reachable and nothing else. Payments are on a public chain.
