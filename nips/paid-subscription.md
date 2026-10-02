# The paid subscription

`draft` `optional`

A relay sells its live feed. A subscriber pays the relay's subscribe route, which opens
a prepaid balance at that relay under a key the subscriber holds, and then dials the
relay and reads events as they arrive. The relay debits the balance by its broadcast price for each event it
broadcasts to that subscriber, and the feed stops when the balance runs out. A reader
who has not paid still gets the stored events that match its filter, and nothing after
them.

## Motivation

A TOON relay is an app behind a connector. A write to it is a paid packet; a read is a
plain NIP-01 websocket and is free. Nothing in that lets one relay receive another
relay's events as they are written, and nothing pays the relay that serves them.

Two designs were turned down:

- **The relay pushes each event to the subscriber as a paid write.** An app cannot
  originate a packet, so the relay could not send one, and somebody would have to pay
  the subscriber's own write price. A subscriber that is a hidden service also cannot
  be dialled.
- **The subscriber pays an amount of its choosing.** A connector tells an app what a
  route charged for a packet and never an amount the payer chose (connector ADR 0040).
  So the subscribe route has a price, each paid packet credits that price, and paying
  more means sending more packets. That is why subscribing and topping up are one
  action here.

The balance belongs to a Nostr key and not to whoever paid. A connector states a payer
to an app only for a claim it admitted itself (connector ADR 0040): a packet that
arrives through another connector, or on the peer wire, states none or states the
intermediary. A balance keyed by that payer could be opened only by a direct client of
the relay's connector, and would be shared by everyone who pays through the same
intermediary. A key the subscriber signs with works over any path.

NIP-11's `fees.subscription` describes admission for a period of time, not a balance
drawn down per event, and no NIP says how a payment made outside the websocket is tied
to a connection on it. NIP-42 and NIP-98 already say how a client proves which key it
holds, and this draft uses both rather than a new message.

## Terms

- **Subscribe route**: a route of the relay's connector, delivered to the relay, whose
  price is credited to a subscription.
- **Subscriber key**: a Nostr key a subscription belongs to. Whoever holds it
  authorizes payments into that subscription, sets its filter, and reads its feed and
  its balance.
- **Subscription**: the balance a relay holds for one subscriber key, with that
  subscription's filter.
- **Subscribe**: pay the subscribe route. The first payment opens a subscription and
  every later one tops it up.
- **Subscriber**: whoever holds a subscriber key that has a subscription.
- **Broadcast price**: what the relay debits from a subscription for each event it
  broadcasts to that subscriber.
- **Live feed**: the events a relay accepts after it has sent `EOSE` on a `REQ`.
- **Free read**: a `REQ` on a connection that holds no subscription, or an exhausted
  one.
- **Exhausted**: said of a subscription whose balance is less than the broadcast
  price.

Every amount in this draft is a JSON integer in the unit of the relay's write price
(`toon.price` in its information document): the settlement asset's base units.

## Specification

### What the relay publishes

A relay that sells its live feed MUST add one object, `toon_subscription`, to its
NIP-11 relay information document, beside the `toon` object that names its paid write
edge (TOON Network spec §13):

| Field | Type | Meaning |
| --- | --- | --- |
| `ilp_address` | string | The address a subscribe packet is sent to, e.g. `g.toon.relay.subscribe` |
| `price` | int | What the subscribe route charges for one packet, and so what one packet credits. Greater than `0` |
| `broadcast_price` | int | What the relay debits for each event it broadcasts to a subscriber. Greater than `0` |
| `carriage` | string? | The carriage the subscribe route pins, `http` or `btp`, with the meaning `toon.carriage` has. Absent when it pins none |

- The subscribe route is terminated by the connector the `toon` object names. A
  subscriber reads `connector_url`, `connector_seal_key` and `settlement` from `toon`.
  It seals each packet to `toon.connector_seal_key`, a 65-byte uncompressed public key
  in hex (it begins `04`, with or without a `0x` prefix), and does not need to reach
  `connector_url`, which is a location hint only: a subscriber that has no peering
  reads it to tell the operator where to peer. A `toon` object without `ilp_address`,
  `connector_url`, `connector_seal_key` or `price`, or whose `connector_seal_key` is not
  such a key, is no write edge, and the subscriber pays nothing.
  A relay that publishes `toon_subscription` MUST publish `toon`.
- The relay MUST read `price` from its connector's self-description, as it does
  `toon.price`: it is the price of the route whose prefix is `ilp_address`. It MUST
  NOT publish `toon_subscription` while its connector does not say it terminates that
  address.
- The subscribe route MUST charge a flat price greater than `0`. A price that grows
  with the size of the packet would make a packet credit something other than `price`,
  and a route that charges nothing would credit nothing. A relay whose connector
  prices the route otherwise MUST NOT publish `toon_subscription`.
- `carriage` comes from the same route of the self-description, as `toon.carriage`
  does from the write route.
- `ilp_address` and `broadcast_price` are the relay's own settings.
- The relay MUST list `42` in `supported_nips`.
- A client MUST ignore a field of `toon_subscription` that this draft does not name.

A relay whose information document has no `toon_subscription` does not sell its feed,
and nothing in this draft says how it answers a `REQ`.

### Subscribing

To subscribe, a subscriber sends a paid packet to `ilp_address` through the relay's
connector, as it would send a write. The request the packet carries is a `POST` to the
route's own path (target `/`) with two headers and a JSON body:

- `Content-Type: application/json`.
- `Authorization: Nostr <base64>`, NIP-98 authorization: a kind `27235` event signed
  by the subscriber key, with a `method` tag of `POST`, a `u` tag that is the HTTP
  form of the relay's URL (the URL its information document is served at), and a
  `payload` tag that is the SHA-256 of the body in lower-case hex. A request inside a
  packet has no URL of its own, so `u` names the relay the payment is meant for.

| Field | Type | Meaning |
| --- | --- | --- |
| `filter` | object? | One NIP-01 filter: the events this subscription pays for |

The subscriber key is the `pubkey` of the authorization event. The relay handles the
request as follows.

1. If the authorization is missing, does not verify, is not of kind `27235`, has a
   `method` other than `POST`, a `u` that is not this relay, a `payload` that is not
   the hash of the body, or a `created_at` more than 60 seconds from the relay's
   clock, it answers `401` with the error code `unauthorized`.
2. If the body is not a JSON object, or `filter` is present and is not a filter the
   relay would accept in a `REQ`, it answers `400` with `invalid_request`.
3. If the subscriber key has no subscription, this is a first payment and `filter` is
   required. If it is missing, it answers `400` with `filter_required`.
4. Otherwise it creates the subscription if there is none, credits the packet to its
   balance, replaces the filter if `filter` is present, and answers `200`.

What a packet credits is what the subscribe route charged for it: the amount in the
`X-TOON-Amount` header when the relay's connector states one, and otherwise the
route's price as that connector's self-description gives it, which is `price`. A relay
MUST credit a packet once and MUST NOT credit a request it refuses. A body of `{}` on
a later payment only tops up.

The relay does not use the payer its connector may state. Any packet the subscribe
route delivers is credited, whichever path it took and whoever paid for it, so a
subscriber can pay through an intermediary, over a peering in either direction, or
have someone else pay. For the same reason a relay MUST accept a subscribe request
only from its own connector: the route's handler is reachable by nothing else.

A relay MUST NOT refuse an authorization because it has seen it before. A subscriber
that pays several packets with the same body MAY sign once and reuse the event while
it is fresh.

The `200` body is the subscription as it now stands:

| Field | Type | Meaning |
| --- | --- | --- |
| `pubkey` | string | The subscriber key: 32 bytes, lower-case hex |
| `credited` | int | What this packet added to the balance |
| `balance` | int | The balance after it |
| `broadcast_price` | int | The broadcast price now |
| `filter` | object | The subscription's filter |

A refusal has the status given above and the body
`{ "error": { "code": "<code>", "message": "<text>" } }`. `code` is stable; `message`
is for a person and may be reworded.

A connector fulfils a packet whenever the app answered, whatever it answered. So a
refusal reaches the subscriber as an HTTP status inside a fulfilled packet, and the
price of that packet is spent. A subscriber SHOULD read the information document and
check its request before it pays, and MUST read the status of every answer.

To pay more than `price`, a subscriber sends more packets. `filter` MAY be repeated on
each of them; repeating the same filter changes nothing.

A filter's `limit` has no meaning for a live feed and is ignored. `since` and `until`
bound `created_at` as in NIP-01.

### The live feed

The live feed is read on the relay's ordinary websocket. A subscriber proves which
subscription it holds with NIP-42: the relay sends an `AUTH` challenge when a
connection opens, and the subscriber answers with an `AUTH` event (kind `22242`) signed
by its subscriber key. A connection authenticated with a key holds the subscription
whose subscriber key that is, if there is one. NIP-42 lets a connection authenticate
with several keys; it holds the subscription of the one it authenticated with last,
and no other.

The relay answers every `REQ` on every connection with the stored events that match,
then `EOSE`, as NIP-01 says. Stored events are never debited. What happens next
depends on the connection:

- **It holds a subscription that is not exhausted.** The `REQ` stays open.
- **It has authenticated, and holds no subscription or an exhausted one.** The relay
  sends `["CLOSED", <subscription id>, "payment-required: <text>"]`.
- **It has not authenticated.** The relay sends
  `["CLOSED", <subscription id>, "auth-required: <text>"]`.

When the relay accepts a new event, then for each subscription that is not exhausted:

1. If the event does not match the subscription's filter, nothing happens.
2. If no open `REQ` on a connection holding that subscription has a filter the event
   matches, nothing happens. An event is never debited to a subscriber who is not
   connected or did not ask for it.
3. Otherwise the relay debits the broadcast price from the balance, once, and sends
   the event on every such `REQ`.

So an event sent on an open `REQ` always matches both that `REQ`'s filters and the
subscription's filter, and costs one broadcast price however many of the subscriber's
`REQ`s or connections it is sent on.

A payment or a change of filter takes effect for events accepted after the relay has
answered it; open `REQ`s stay open.

`payment-required:` is a `CLOSED` prefix this draft adds to those of NIP-01 and
NIP-42. A client MUST branch on the prefix and not on the text after it. The prefix
says only that this connection holds nothing that pays for the feed. A subscriber
whose `REQ` was open and is closed this way has run out; one that needs to know more
reads its balance.

### When the balance runs out

As soon as a subscription is exhausted, the relay MUST close every open `REQ` on every
connection holding it with
`["CLOSED", <subscription id>, "payment-required: <text>"]`. This happens directly
after the last event the balance paid for, not when the next event arrives. The relay
leaves the connection open.

Whatever is left of the balance stays in the subscription and counts toward the next
payment. To resume, the subscriber subscribes again and sends its `REQ` again.

The same holds after a dropped connection or a restart. A relay broadcasts events in
the order it accepts them, which is not the order of `created_at`, so a subscriber
that wants what it missed sets `since` some way before the newest `created_at` it has
seen and drops the events whose ids it already has. What it missed comes back as
stored events and is not debited. Events the relay does not keep are the exception:
an ephemeral event, and a replaceable or addressable event that a newer one replaced
in the meantime, cannot be read afterwards.

### Reading the balance

A subscriber reads its subscription with a `GET` of the HTTP form of the relay's URL,
the URL the information document is served at, with the header
`Accept: application/toon-subscription+json` and NIP-98 authorization: an
`Authorization: Nostr <base64>` header carrying a kind `27235` event signed by the
subscriber key, whose `u` tag is that URL and whose `method` tag is `GET`. It is the
authorization a payment carries, with the other method and no `payload`.

- `200`, with `Content-Type: application/toon-subscription+json`: the body has
  `pubkey`, `balance`, `broadcast_price` and `filter`, as in the answer to a payment.
- `401`, error code `unauthorized`: the authorization is missing or does not verify.
- `404`, error code `not_subscribed`: the key has no subscription, either because
  there never was one or because the relay forgot an exhausted one.

Reading the balance is free and changes nothing.

A relay checks the `relay` tag of a NIP-42 event and the `u` tag of a NIP-98 event
against the URLs it is reached at. NIP-98 asks for the exact URL of the request. A
relay that is a hidden service, or that its operator serves through a reverse proxy,
cannot see which URL the client used, so here a relay MAY accept a `u` whose host is
one of its own, as NIP-42 already allows for `relay`.

### A free read

A free read returns the stored events that match, then `EOSE`, then `CLOSED`, as
above. The relay MUST NOT send an event it accepted after `EOSE` on a `REQ` whose
connection holds no subscription that is not exhausted, with one exception.

A relay MAY broadcast to a connection authenticated with a key its operator has named,
without a subscription and without a debit. This is how an operator reads the live
feed of their own relay without paying themselves. How the operator names the key is
the relay's business.

### Balances

- A relay holds one balance per subscriber key. Balances at different relays have
  nothing to do with each other, and a balance cannot be moved from one key to
  another.
- A balance is not refundable. Nothing in this draft lets a subscriber withdraw one,
  and a relay owes nothing for a balance it holds when the subscriber stops reading.
- A debit is for a broadcast, not for a receipt. An event that was debited and did not
  arrive, because the connection dropped, can be read for free if the relay stores it.
- A debit is made at the broadcast price in force when the event is broadcast. A
  balance is an amount of money, not a number of events.
- A relay MUST NOT forget a subscription that is not exhausted. It MAY forget an
  exhausted one, and the next payment under that key is then a first payment. A
  subscriber that is not sure its subscription still exists SHOULD send `filter` with
  its payment.

## Kinds

None new.

| Kind | Event | Class | Signed with | Defined by |
| --- | --- | --- | --- | --- |
| `22242` | Client authentication | ephemeral | Subscriber key | NIP-42 |
| `27235` | HTTP authorization | ephemeral | Subscriber key | NIP-98 |

A later revision that needs a kind takes it under TOON Network's rule (its ADR 0012
and spec §3.1): one contiguous block per NIP-01 class, the lowest free number of the
block for that class, recorded in that spec's table.

## Examples

A relay at `wss://relay.example` sells its feed. Its information document, in part:

```json
{
  "supported_nips": [1, 9, 11, 16, 40, 42],
  "toon": {
    "ilp_address": "g.toon.relay",
    "connector_url": "https://c.relay.example/ilp",
    "connector_seal_key": "04a1…",
    "price": 100,
    "settlement": […]
  },
  "toon_subscription": {
    "ilp_address": "g.toon.relay.subscribe",
    "price": 1000,
    "broadcast_price": 10
  }
}
```

One packet costs `1000` and pays for a hundred events.

**The first payment.** The subscriber sends a packet to `g.toon.relay.subscribe`
carrying:

```http
POST / HTTP/1.1
Content-Type: application/json
Authorization: Nostr <base64 of the event below>

{"filter":{"kinds":[1],"authors":["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"]}}
```

```json
{"kind": 27235, "pubkey": "7e7e9c42…", "tags": [["u", "https://relay.example/"], ["method", "POST"], ["payload", "<SHA-256 of the body, in hex>"]], "content": "", …}
```

The connector delivers it to the relay, here with `X-TOON-Amount: 1000`. The packet is
fulfilled, and the answer it carries is:

```json
{
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",
  "credited": 1000,
  "balance": 1000,
  "broadcast_price": 10,
  "filter": { "kinds": [1], "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"] }
}
```

**Paying again.** Two more packets with the body `{}`, authorized the same way with
the hash of that body, each answer `"credited": 1000`, and the second answers
`"balance": 3000`.

**The live feed.** The subscriber connects to `wss://relay.example`:

```
relay:      ["AUTH", "f3a9c1"]
subscriber: ["AUTH", {"kind": 22242, "pubkey": "7e7e9c42…", "tags": [["relay", "wss://relay.example"], ["challenge", "f3a9c1"]], …}]
relay:      ["OK", "<id of the AUTH event>", true, ""]
subscriber: ["REQ", "feed", {"kinds": [1], "since": 1790000000}]
relay:      ["EVENT", "feed", {…a stored event…}]
relay:      ["EOSE", "feed"]
relay:      ["EVENT", "feed", {…an event written just now…}]
```

The balance is now `2990`. An event of kind `7`, or a note by another author, is not
sent and not debited.

**Running out.** The three-hundredth live event leaves a balance of `0`:

```
relay:      ["EVENT", "feed", {…}]
relay:      ["CLOSED", "feed", "payment-required: the subscription's balance has run out"]
```

**Reading the balance.**

```http
GET / HTTP/1.1
Host: relay.example
Accept: application/toon-subscription+json
Authorization: Nostr <base64 of the event below>
```

```json
{"kind": 27235, "pubkey": "7e7e9c42…", "tags": [["u", "https://relay.example/"], ["method", "GET"]], "content": "", …}
```

The relay answers `200`:

```json
{
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",
  "balance": 0,
  "broadcast_price": 10,
  "filter": { "kinds": [1], "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"] }
}
```

**A free read.** A connection that never authenticates:

```
relay:      ["AUTH", "09be7d"]
reader:     ["REQ", "q", {"kinds": [1], "limit": 20}]
relay:      ["EVENT", "q", {…}]
relay:      ["EOSE", "q"]
relay:      ["CLOSED", "q", "auth-required: the live feed is for subscribers"]
```

**A refusal.** A first payment with the body `{}`, correctly authorized, is fulfilled,
costs `1000`, and answers `400`:

```json
{ "error": { "code": "filter_required", "message": "a first payment needs a filter" } }
```

## Limits

- **The relay is trusted with the balance.** Nothing proves to a third party what a
  relay holds or what it debited. A subscriber limits what it can lose by paying a
  little at a time.
- **The prices are a statement, not a contract.** An information document is not
  signed, and a relay may change its broadcast price while it holds a balance. A
  subscriber sees the price in force in every answer to a payment and every read of
  its balance.
- **A refused request still costs its price.** The connector charges for a packet the
  relay answered, whatever the answer.
- **The balance is the key's.** Whoever holds the subscriber key can pay into the
  subscription, change its filter and draw it down. A subscriber that loses the key
  loses the balance, and nobody else can recover it.
- **The relay may learn who pays for which key.** When its connector states a payer,
  the relay sees it beside the subscriber key. A subscriber that signs public events
  with one key and does not want its payments tied to them SHOULD use a subscriber
  key made for this purpose.
- **The relay trusts its connector for the credit.** It credits what the connector
  says it charged, or the route's price when the connector says nothing, and cannot
  check either. It trusts the same connector for every write.
- **An authorization can be replayed while it is fresh.** Each replay is a packet
  somebody paid for and credits the key it names, so it gives away money. The most
  it takes is to put back a filter the subscriber replaced within that minute.
- **A reader can poll instead of paying.** Stored events are free, so repeated free
  reads approximate the feed. A relay that sells its feed needs a rate limit on free
  reads; that limit is not part of this draft.
## Open questions

- **One filter.** A subscription has one filter, where a `REQ` takes several. A list
  would let one subscription cover what today needs a broad filter and narrower
  `REQ`s. The lean is to keep one until a subscriber needs more.
- **No number.** A draft has no NIP number, so a relay cannot list it in
  `supported_nips`. The lean is to leave it so: the presence of `toon_subscription`
  is how a client tells that a relay implements this.
