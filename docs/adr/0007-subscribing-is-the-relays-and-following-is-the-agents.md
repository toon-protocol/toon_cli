# Subscribing is the relay's, and following is the agent's

In Nostr a client does both halves of reading: its follow list says whose events it wants, and its subscriptions fetch them from many relays. Here every agent node has its own relay, and a subscription already fills that relay (ADR 0005). So the two halves separate. Subscribing is what the agent node's relay does to get events from another relay: paid, per relay, and decided by the operator. Following is what the agent does: its follow list is its own record of whose events it wants, and becomes the `authors` of a filter when it reads its own relay.

The two are joined only by an explicit command. `--following` builds a filter from the follow list, on a read and on `toon relay subscribe`. A subscription's filter made that way is a snapshot, and changing the follow list changes no subscription. We chose this over subscribing automatically for whoever is followed, which would pay without a command, and over letting the agent subscribe to other relays directly as a Nostr client does, which would bypass its own relay and undo ADR 0005.

"Follow" therefore names the follow list and nothing else. `toon event follow`, which printed another relay's feed over a second connection, is replaced by `toon event watch`, a live read of the agent node's own relay.

## Consequences

- An agent reads its feed in one place, its own relay, whatever relays the events came from.
- A newly followed key delivers nothing until the operator subscribes again with the new list. The report of `relay subscribe --following` says so.
- `event watch` needs no subscription: the relay gives its operator a live read, and recognises the operator by NIP-42, not by address (relay #259, already implemented). A reader that arrives through the overlay comes from loopback too, so address would not do. The CLI answers the relay's challenge with the relay's own identity key, which it holds, so nothing is set in `TOON_OPERATOR_PUBKEYS`.
- Removing `event follow` breaks a command of v0.1.0. It is removed outright, with no alias, which could not be faithful: `follow` took another relay's URL and `watch` takes none. Running it is `usage` and names `watch`.
- Watching another relay's feed directly is not wanted. A relay debits one broadcast price per event per subscriber key however many connections receive it, so the second connection of `event follow` was never a double charge.
- Which relays to subscribe at stays the operator's choice. Reading followed keys' relay lists to suggest them is not part of this.
- There is still no command per NIP: the follow list is published with `toon event publish`.
