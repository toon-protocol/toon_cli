# A subscription is a paid live feed

A relay sells its live feed. A free read returns stored events and stops; events that arrive afterwards are broadcast only to a subscriber with a balance, which the relay debits by its broadcast price per event until it runs out. We chose this over pushing events into the subscriber's relay as packets, which would need an app to originate packets (it cannot) and someone to pay the subscriber's own write price. A feed the subscriber dials also works from behind a hidden service, where nothing can dial in.

The connector tells an app only what a route charged, never an amount the payer chose (connector ADR 0040). So the subscribe route has a price, each paid packet credits exactly that price, and paying more means sending more packets. That is why subscribing and topping up are one action.

The balance belongs to a subscriber key, a Nostr key the subscriber signs each payment with, and not to the payer the connector states. We first keyed it by that payer. The connector states one only for a claim it admitted itself, so a packet that arrives through another connector, or on the peer wire once the relay's operator has created a peering in return, states none or states the intermediary's. A balance keyed that way could be opened only by a direct client of the relay's connector and would be shared by everyone paying through the same intermediary. The relay instead credits every packet the subscribe route delivers to the key that signed its request, and the subscriber proves it holds that key when it reads the feed.

## Consequences

- The serving side is relay work, in the Rust relay only (relay #185 freezes TypeScript features): the route, a ledger keyed by the subscriber key, metering, and the published broadcast price.
- The receiving side lives in this CLI's supervisor, not in the relay, which drops its outbound subscriber in the rewrite. The supervisor dials the other relay through the overlay and hands each event to its own relay's write endpoint over loopback.
- A client can poll free reads instead of paying, so free reads need a rate limit for the feed to be worth buying.
- An agent node pays through its own connector, so subscribing to a relay it has no path to means peering first.
- Balances are not refundable.
- The relay trusts its connector to deliver to the subscribe route only what it charged for, as it does for writes. The contract is the draft NIP, `nips/paid-subscription.md`.
- The wallet derives the subscriber key at `m/10473'/6'/0'`, once per wallet, and not from the agent identity: a relay can see which payments go into which subscriber key, and the agent identity's events are public.
- The pinned connector's `send` cannot carry a request header, and a subscribe request needs `Authorization`. The CLI therefore forms, seals and signs that packet itself (`operator::dispatch_with_headers`), with the connector's own crates.
