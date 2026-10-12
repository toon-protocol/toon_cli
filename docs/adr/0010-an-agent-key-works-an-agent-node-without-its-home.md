# An agent key works an agent node without its home

Every `toon` command read the agent node's home, so wherever an operator ran `toon` the wallet, the connectors' keys and the agent identity's secret were too. An agent that reads strangers' notes should not sit beside the money. So the operator hands such an agent an agent key: an operator write key with a scope and a spending limit. With it `toon` runs with no home and no keystore, and sends each command either as a signed write to a connector's operator surface or as an order, a signed request, to the supervisor.

Three choices shape this.

**The supervisor signs for an agent key and never pays for it.** To publish an event or send a private message, `toon` asks the supervisor to sign with the agent identity it already keeps (ADR 0008), then sends the packets itself with the agent key. We chose this over the supervisor sending them: those packets would leave under the supervisor's own operator key, the connector would not count them against the agent key, and the supervisor would need a second count of its own. No order moves money, so the connector is the one place an agent key's spending limit is enforced.

**The home holds the record of agent keys.** Each key's scope, spending limit, expiry and the TOON apps it is granted on are written in the home, and `toon` writes each connector's list of write keys from that record. We chose this over the supervisor reading a connector's list: that list would carry scopes for orders the connector does not understand, and an agent node may have several connectors. Revoking a key removes it from the record and from every connector at once.

**What an agent key may add is the operator's to approve.** An image on the operator's list of trusted images is added at once. Any other image, and every app at a URL, becomes a pending app that the supervisor does not start until the operator approves it. An app at a URL is always held because the connector would otherwise deliver to any address the agent names, including ones only the connector can reach.

## Consequences

- An agent key with the signing scope can sign anything as the agent identity. The secret stays in the home and the key can be revoked, but what is signed is not bounded by kind.
- A key is granted per TOON app and its spending limit is per connector. A key granted on three TOON apps can spend three times its daily limit. A total across the agent node would put the supervisor in the path of every payment.
- The supervisor listens for orders on loopback only (ADR 0003). An agent in a place that cannot reach the machine's loopback needs a tunnel the deployer provides.
- The wallet's spending limit still bounds commands run with the home. An agent key's is a different limit, kept by the connector, so the two are not one total.
- A command that needs the keystore or stops the agent node is refused with an agent key: `init`, `wallet`, `backup`, `restore`, `limit set`, `create`, `up`, `down`, and the commands that manage agent keys and pending apps.
- This depends on the connector giving each write key a scope and a spending limit, re-reading its list without a restart, and accepting a signed read in place of the bearer token (toon-protocol/connector#1485).
