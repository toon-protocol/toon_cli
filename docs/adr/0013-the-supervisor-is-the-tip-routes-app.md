# The supervisor is the tip route's app

A tip arrives at a route that is paid whatever arrives (connector ADR 0083), and the app behind that route has to answer it with a tip acknowledgement signed with the agent identity (relay ADR 0007). Only the supervisor holds that secret without a passphrase (ADR 0008), and an app is confined and given nothing it is not allowed (ADR 0011). So the supervisor itself is the app behind an agent node's tip route: it listens on loopback, and the connector delivers the tip there as it delivers a write to the relay.

We chose this over a tip app in a container, which would need the agent identity's secret as one of its secrets (ADR 0012) or a way to ask the supervisor to sign for it. The first gives an app the key every event of the agent's is signed with; the second is the supervisor doing the work anyway, with a container in between.

An acknowledgement is only read where it is published, and the readers that count tips read the network's relay, not the agent node's own. So the supervisor publishes each acknowledgement to the agent node's own relay and to the relay its profile follows, and pays for the second write itself, with the connector's keys that the home already keeps. It is the first thing the supervisor pays for on its own.

## Consequences

- A tip is fulfilled whatever the supervisor answers (connector ADR 0019), so the money has arrived before anything is published. The supervisor publishes an acknowledgement only for a tip receipt that is signed, names this agent identity and states the unit that arrived, and only once per receipt; any other tip is kept and not acknowledged, and the reason is logged.
- An acknowledgement that cannot be published, because a relay is down or the write cannot be paid, is kept in the home and tried again, and `toon status` says how many wait and why. A tip is never acknowledged nowhere without the operator being able to see it.
- The supervisor's payments count against the spending limit like the operator's.
- An agent node takes tips only after its operator enables them. Enabling adds the route and publishes the tip route event; disabling removes the route and deletes the event.
- Tips reach an agent node only over peerings. A client payer or a client recipient is not covered.
