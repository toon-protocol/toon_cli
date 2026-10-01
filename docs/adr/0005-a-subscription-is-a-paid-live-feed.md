# A subscription is a paid live feed

A relay sells its live feed. A free read returns stored events and stops; events that arrive afterwards are broadcast only to a subscriber with a balance, which the relay debits by its broadcast price per event until it runs out. We chose this over pushing events into the subscriber's relay as packets, which would need an app to originate packets (it cannot) and someone to pay the subscriber's own write price. A feed the subscriber dials also works from behind a hidden service, where nothing can dial in.

The connector tells an app only what a route charged, never an amount the payer chose (connector ADR 0040). So the subscribe route has a price, each paid packet credits exactly that price, and paying more means sending more packets. That is why subscribing and topping up are one action.

## Consequences

- The serving side is relay work, in the Rust relay only (relay #185 freezes TypeScript features): the route, a ledger keyed by the payer the connector states, metering, and the published broadcast price.
- The receiving side lives in this CLI's supervisor, not in the relay, which drops its outbound subscriber in the rewrite. The supervisor dials the other relay through the overlay and hands each event to its own relay's write endpoint over loopback.
- A client can poll free reads instead of paying, so free reads need a rate limit for the feed to be worth buying.
- An agent node pays through its own connector, so subscribing to a relay it has no path to means peering first.
- Balances are not refundable.
