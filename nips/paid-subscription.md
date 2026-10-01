# The paid subscription

`draft` `optional`

A relay sells its live feed. A subscriber pays the relay's subscribe route, which opens
a prepaid balance at that relay, and then dials the relay and reads events as they
arrive. The relay debits the balance by its broadcast price for each event it
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

NIP-11's `fees.subscription` describes admission for a period of time, not a balance
drawn down per event, and no NIP says how a payment made outside the websocket is tied
to a connection on it. NIP-42 and NIP-98 already say how a client proves which key it
holds, and this draft uses both rather than a new message.

## Terms

- **Subscribe route**: a route of the relay's connector, delivered to the relay, whose
  price is credited to a subscription.
- **Payer**: what the relay's connector states as the payer of a packet it delivers:
  the `X-TOON-Payer` header of connector ADR 0040, for example
  `evm:0x<64 lower-case hex>` or `solana:<base58>`. It names a channel toward that
  connector. The relay treats it as an opaque string.
- **Subscription**: the balance a relay holds for one payer, with the filter and the
  subscriber key that payer set.
- **Subscribe**: pay the subscribe route. The first payment opens a subscription and
  every later one tops it up.
- **Subscriber key**: the Nostr public key a payer names when it subscribes. Whoever
  proves they hold it reads that subscription's feed and its balance.
- **Subscriber**: whoever holds a subscription: it pays as the payer and reads with
  the subscriber key.
- **Broadcast price**: what the relay debits from a subscription for each event it
  broadcasts to that subscriber.
- **Live feed**: the events a relay accepts after it has sent `EOSE` on a `REQ`.
- **Free read**: a `REQ` from a connection that holds no subscription.
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
  A relay that publishes `toon_subscription` MUST publish `toon`.
- The relay MUST read `price` from its connector's self-description, as it does
  `toon.price`: it is the price of the route whose prefix is `ilp_address`. It MUST
  NOT publish `toon_subscription` while its connector does not say it terminates that
  address.
- The subscribe route MUST charge a flat price greater than `0`. A price that grows
  with the size of the packet would make a packet credit something other than `price`,
  and a connector states no payer on a route that charges nothing. A relay whose
  connector prices the route otherwise MUST NOT publish `toon_subscription`.
- `ilp_address` and `broadcast_price` are the relay's own settings.
- The relay MUST list `42` in `supported_nips`.
- A client MUST ignore a field of `toon_subscription` that this draft does not name.

A relay whose information document has no `toon_subscription` does not sell its feed,
and nothing in this draft says how it answers a `REQ`.

### Subscribing

To subscribe, a subscriber sends a paid packet to `ilp_address` through the relay's
connector, as it would send a write. The request the packet carries is a `POST` to the
route's own path (target `/`) with `Content-Type: application/json` and this body:

| Field | Type | Meaning |
| --- | --- | --- |
| `filter` | object? | One NIP-01 filter: the events this subscription pays for |
| `pubkey` | string? | The subscriber key: 32 bytes, lower-case hex |

The relay handles the request as follows.

1. It reads the payer and the amount from the `X-TOON-Payer` and `X-TOON-Amount`
   headers its connector set. If either is absent or malformed, it answers `403` with
   the error code `payer_not_stated` and credits nothing.
2. If the body is not a JSON object, or `filter` is present and is not a filter the
   relay would accept in a `REQ`, or `pubkey` is present and is not 64 lower-case hex
   characters, it answers `400` with `invalid_request`.
3. If the payer has no subscription, this is a first payment: `filter` and `pubkey`
   are both required. If either is missing, it answers `400` with `not_subscribed`.
4. If `pubkey` is present and is the subscriber key of another payer's subscription,
   it answers `409` with `pubkey_in_use`.
5. Otherwise it creates the subscription if there is none, adds the amount the
   connector stated to its balance, replaces the filter if `filter` is present,
   replaces the subscriber key if `pubkey` is present, and answers `200`.

A relay MUST credit exactly the amount in `X-TOON-Amount`, once per packet, and MUST
NOT credit a request it refuses. A field that is absent on a later payment leaves that
part of the subscription as it was, so a body of `{}` only tops up.

The `200` body is the subscription as it now stands:

| Field | Type | Meaning |
| --- | --- | --- |
| `payer` | string | The payer the connector stated |
| `credited` | int | What this packet added to the balance |
| `balance` | int | The balance after it |
| `broadcast_price` | int | The broadcast price now |
| `filter` | object | The subscription's filter |
| `pubkey` | string | The subscriber key |

A refusal has the status given above and the body
`{ "error": { "code": "<code>", "message": "<text>" } }`. `code` is stable; `message`
is for a person and may be reworded.

A connector fulfils a packet whenever the app answered, whatever it answered. So a
refusal reaches the subscriber as an HTTP status inside a fulfilled packet, and the
price of that packet is spent. A subscriber SHOULD read the information document and
check its request before it pays, and MUST read the status of every answer.

To pay more than `price`, a subscriber sends more packets. `filter` and `pubkey` MAY be
repeated on each of them; repeating the same values changes nothing.

A filter's `limit` has no meaning for a live feed and is ignored. `since` and `until`
bound `created_at` as in NIP-01.

### Who the payer is

A payer is a channel toward the relay's connector, and a connector states one only for
a packet admitted on a claim it verified on that channel (connector ADR 0040). Three
things follow, and a subscriber MUST take them into account before it pays.

- A subscriber MUST pay the relay's connector directly, as a client, on a channel of
  its own. An agent node does this over a peering it created toward that connector.
- A packet that reaches the relay's connector through another connector states either
  no payer, in which case the relay answers `payer_not_stated` and the price is spent,
  or that other connector's channel, in which case the subscription belongs to whoever
  else pays through it. A subscriber MUST NOT subscribe through an intermediary.
- A subscriber that pays from a second channel is a second payer and opens a second
  subscription. A balance cannot be moved from one payer to another.

A subscriber SHOULD send one packet first and check that `payer` in the answer is its
own channel before it sends the rest.

### The live feed

The live feed is read on the relay's ordinary websocket. A subscriber proves which
subscription it holds with NIP-42: the relay sends an `AUTH` challenge when a
connection opens, and the subscriber answers with an `AUTH` event (kind `22242`) signed
by its subscriber key. A connection authenticated with a key holds the subscription
whose subscriber key that is, if there is one.

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
answered it; open `REQ`s stay open. After a change of subscriber key, a connection
authenticated with the earlier key no longer holds the subscription, and the relay
closes its open `REQ`s with `payment-required:`.

`payment-required:` is a `CLOSED` prefix this draft adds to those of NIP-01 and
NIP-42. A client MUST branch on the prefix and not on the text after it.

### When the balance runs out

As soon as a subscription is exhausted, the relay MUST close every open `REQ` on every
connection holding it with
`["CLOSED", <subscription id>, "payment-required: <text>"]`. This happens directly
after the last event the balance paid for, not when the next event arrives. The relay
leaves the connection open.

Whatever is left of the balance stays in the subscription and counts toward the next
payment. To resume, the subscriber subscribes again and sends its `REQ` again, with
`since` set to the last event it received; events it missed in between are stored
events and are not debited.

### Reading the balance

A subscriber reads its subscription with a `GET` of the HTTP form of the relay's URL,
the URL the information document is served at, with the header
`Accept: application/toon-subscription+json` and NIP-98 authorization: an
`Authorization: Nostr <base64>` header carrying a kind `27235` event signed by the
subscriber key, whose `u` tag is that URL and whose `method` tag is `GET`.

- `200`, with `Content-Type: application/toon-subscription+json`: the body has
  `payer`, `balance`, `broadcast_price`, `filter` and `pubkey`, as in the answer to a
  payment.
- `401`, error code `unauthorized`: the authorization is missing or does not verify.
- `404`, error code `not_subscribed`: the key is no subscription's subscriber key.

Reading the balance is free and changes nothing.

A relay checks the `relay` tag of a NIP-42 event and the `u` tag of a NIP-98 event
against the URLs it is reached at. A relay that is a hidden service, or sits behind a
proxy, cannot see which URL the client used, so matching the host is enough.

### A free read

A free read returns the stored events that match, then `EOSE`, then `CLOSED`, as
above. The relay MUST NOT send an event it accepted after `EOSE` on a `REQ` whose
connection holds no subscription that is not exhausted, with one exception.

A relay MAY broadcast to a connection authenticated with a key its operator has named,
without a subscription and without a debit. This is how an operator follows their own
relay. How the operator names the key is the relay's business.

### Balances

- A relay holds one balance per payer. Balances at different relays have nothing to do
  with each other.
- A balance is not refundable. Nothing in this draft lets a subscriber withdraw one,
  and a relay owes nothing for a balance it holds when the subscriber stops reading.
- A debit is for a broadcast, not for a receipt. An event that was debited and did not
  arrive, because the connection dropped, is still stored and can be read for free.
- A debit is made at the broadcast price in force when the event is broadcast. A
  balance is an amount of money, not a number of events.
- A relay MUST NOT forget a subscription that is not exhausted. It MAY forget an
  exhausted one, and the next packet from that payer is then a first payment. A
  subscriber that is not sure its subscription still exists SHOULD send `filter` and
  `pubkey` with its payment.

## Kinds

None new.

| Kind | Event | Class | Signer | Defined by |
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

{
  "filter": { "kinds": [1], "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"] },
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"
}
```

The connector delivers it to the relay with `X-TOON-Payer: evm:0x5c3b…e1f2` and
`X-TOON-Amount: 1000`. The packet is fulfilled, and the answer it carries is:

```json
{
  "payer": "evm:0x5c3b…e1f2",
  "credited": 1000,
  "balance": 1000,
  "broadcast_price": 10,
  "filter": { "kinds": [1], "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"] },
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"
}
```

**A top-up.** Two more packets with the body `{}` each answer `"credited": 1000`, and
the second answers `"balance": 3000`.

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
  "payer": "evm:0x5c3b…e1f2",
  "balance": 0,
  "broadcast_price": 10,
  "filter": { "kinds": [1], "authors": ["3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"] },
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"
}
```

**A free read.** A connection that never authenticates:

```
reader:     ["REQ", "q", {"kinds": [1], "limit": 20}]
relay:      ["EVENT", "q", {…}]
relay:      ["EOSE", "q"]
relay:      ["CLOSED", "q", "auth-required: the live feed is for subscribers"]
```

**A refusal.** A first payment with the body `{}` is fulfilled, costs `1000`, and
answers `400`:

```json
{ "error": { "code": "not_subscribed", "message": "a first payment needs a filter and a pubkey" } }
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
- **The relay learns who pays for which key.** It sees the payer and the subscriber
  key together. A subscriber that signs public events with one key and does not want
  its payments tied to them SHOULD use a subscriber key made for this purpose.
- **A subscriber key that is known can be taken.** A relay refuses a key that another
  payer's subscription already names, so someone who learns a key before its owner
  subscribes can subscribe with it first. The owner loses nothing but has to choose
  another key. A key made for this purpose and not published cannot be taken.
- **A reader can poll instead of paying.** Stored events are free, so repeated free
  reads approximate the feed. A relay that sells its feed needs a rate limit on free
  reads; that limit is not part of this draft.
- **The subscriber key reads the feed that the payer pays for.** Whoever holds the key
  can draw the balance down. Only the payer can change the key.

## Open questions

- **A subscriber has to be a direct client of the relay's connector.** The balance is
  keyed by the payer the connector states, and a connector states one only for a
  claim it admitted itself. That excludes a subscriber that reaches the relay through
  another connector, and one whose connector the relay's connector treats as a peer,
  because a packet on the peer wire states no payer. The alternative is to key the
  balance by the subscriber key and credit every packet the subscribe route delivers,
  whoever paid it; the relay would then trust that its connector delivers only what
  it charged for, which it already does for writes. This draft keeps the payer,
  because that is the decision on record (toon_cli ADR 0005), and it is the first
  thing to settle before a relay implements it.
- **One filter.** A subscription has one filter, where a `REQ` takes several. A list
  would let one subscription cover what today needs a broad filter and narrower
  `REQ`s.
- **No number.** A draft has no NIP number, so a relay cannot list it in
  `supported_nips`. Until it has one, the presence of `toon_subscription` is the only
  way to tell that a relay implements it.
