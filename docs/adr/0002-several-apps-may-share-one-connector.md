# Several apps may share one connector

An operator can add an app behind a connector that already exists, or create a new connector and put the app behind that. `infra` decided the opposite for the devnet (infra ADR 0001: every node keeps its own connector, and nodes are never merged into a shared one), because there each app belongs to a different deployment with its own published seal key. An agent node has one operator, so sharing a connector costs nothing in trust and saves a second set of keys, a second funded channel and a peering.

## Consequences

- A new connector is for an app that needs its own identity, prices or peerings. It gets its own keys and is peered with the connector it was created from unless the operator says otherwise.
- Adding an app changes the connector's route table, which the connector reads only at start, so adding an app restarts that connector.
