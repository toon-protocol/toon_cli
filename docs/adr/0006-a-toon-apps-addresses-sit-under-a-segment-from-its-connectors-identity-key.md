# A TOON app's addresses sit under a segment from its connector's identity key

Every ILP address a TOON app answers to is `g.toon.<segment>` or sits under it, where the address segment is the first 16 hex characters of its connector's identity public key. The relay of the first TOON app is `g.toon.<segment>.relay`, and an app added behind a connector is `g.toon.<segment>.<app>` unless the operator gives it another address.

Before this, an address was made of the name the operator chose: the first TOON app was `g.toon.relay` on every agent node, which is also the address of the network's relay on the sandbox and the devnet. After `toon join` the agent node's own route for `g.toon.relay` was longer than the `g.toon` it forwards to the network, so a packet for the network's relay was delivered to the agent node's own, and no two agent nodes could address each other's (issue #72).

We derive the segment rather than ask the operator for a name because two operators can choose the same name and nothing arbitrates between them: there is no registry, and `init` runs without one. A derived segment needs no input, and the mnemonic alone brings it back, because the connector identity key is derived from it.

The segment is per connector, not per agent node. One shared by every TOON app of an agent node would link them to each other, which is what giving each connector its own onion endpoint is meant to prevent (ADR 0003).

## Consequences

- An address is not readable. The commands that describe a TOON app or its routes show it, and the operator copies it from there.
- An address states which connector identity key it belongs to, and the connector already publishes that key to anyone who sends it a packet. It says nothing about the wallet's other keys.
- 16 hex characters are 64 bits. That is enough that two honest connectors do not collide, and not enough to stop someone grinding a key for a chosen segment. An address is not a proof of identity: a packet goes where the routes of the connectors on its path send it, and what is sealed to a connector is sealed to its whole key.
- The name the operator gives a TOON app stays what every command refers to it by. It is no longer part of an address.
- `g.toon` stays the prefix an agent node forwards to the network it joined. Its own addresses are longer, so they are delivered locally, and no other address under `g.toon` is.
