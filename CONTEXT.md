# TOON CLI

The operator's tool for running an agent node and managing what it holds, charges and connects to. Terms that the connector already defines keep the connector's meaning.

## Language

### What is run

**Agent node**:
Everything one operator runs through the CLI on one machine: one wallet and one or more TOON apps. It starts as a single TOON app whose only app is a relay. Always two words.
_Avoid_: Node, hub, stack, full node

**TOON app**:
One connector together with the apps behind it. "The relay TOON app" is the one whose connector fronts the relay.
_Avoid_: Node, deployment, service; using "TOON app" for an app on its own

**Connector**:
The paid reverse proxy at the front of a TOON app: it accepts a packet, charges for it, and delivers it to an app or forwards it to a peer.
_Avoid_: Gateway, proxy, terminator

**App**:
A payment-oblivious HTTP service that a connector delivers to at the end of a route. The relay is an app. Never "TOON app", which is the connector and its apps together.
_Avoid_: Backend, service

**Relay**:
The Nostr relay app: paid to write to through its connector. An agent node has one, the one `toon init` creates; `toon create` and `toon add` refuse the relay's image.

**Supervisor**:
The one process per machine that runs an agent node: it starts each connector as a child process of the same binary and keeps it running.
_Avoid_: Daemon, manager, orchestrator

**Operator**:
Whoever runs an agent node: a human, or an agent acting through the CLI.
_Avoid_: Admin, user

### What the operator does

**Add**:
Put a new app behind the connector of a TOON app that already exists.
_Avoid_: Spawn, deploy, attach

**Create**:
Start a new TOON app: a new connector with its own identity and keys, and an app behind it.
_Avoid_: Spawn, deploy

**Peering**:
A connector's standing arrangement to forward packets to another connector, paid on a channel it opens toward that connector. An operator creates one alone; the other connector forwards back only if its own operator creates one in return.
_Avoid_: Half-open peering, invite, handshake

**Spending limit**:
The most a wallet will pay out, per command and per day. Every command that moves money states its amount and is refused past the limit.
_Avoid_: Cap (the connector's word for the largest packet a peering carries), budget

**Wallet**:
The operator's keys and funds for an agent node, which the CLI generates and manages. Distinct from the connector's Signer, which only reads the keys it is handed.
_Avoid_: Signer (for this), keystore (for the concept)

**Agent identity**:
The Nostr key an agent signs its events with. It belongs to the wallet but is a different key from any that pays or settles.
_Avoid_: Account, payer, relay identity

### Subscribing

**Subscription**:
A prepaid balance a relay holds for a subscriber, drawn down by the broadcast price for each event the relay broadcasts to that subscriber. It ends when the balance runs out.
_Avoid_: Mirror, sync, follow

**Subscribe**:
Pay a relay's subscribe route. The first payment opens a subscription; every later one tops it up.
_Avoid_: Top up (as a separate action)

**Packet amount**:
What one packet of a subscribe is sent for. It is the subscribe price unless the operator states more, because a connector between the subscriber and the relay may charge to forward. A packet credits what the relay's subscribe route charged, whatever it was sent for.
_Avoid_: Fee

**Subscribe price**:
What a relay's subscribe route charges for one packet, and so what the packet credits. Set by that relay's operator.

**Broadcast price**:
What a relay debits from a subscription for each event it broadcasts. Set by that relay's operator.
_Avoid_: Read price, subscription fee

**Subscriber key**:
The Nostr key a subscription belongs to. A subscriber signs each payment of the subscribe route with it, and proves it holds it when it reads that subscription's live feed or its balance. A relay holds one balance per subscriber key, whoever paid.
_Avoid_: Token, credential, payer (the payer is the channel that paid, and a subscription does not depend on it)

**Follow list**:
The event in which an agent names the keys whose events it wants, signed with its agent identity. It brings no events by itself: it becomes the `authors` of a filter when the agent reads its own relay, or when the operator subscribes with it.
_Avoid_: Subscription, contacts, following a relay

### How it is reached

**ILP address**:
Where a packet is sent: `g.toon`, then the address segment of a TOON app, then the app behind its connector. It says which app a packet is for, not where the connector is dialled, which is the onion endpoint or the clearnet hostname.
_Avoid_: Address on its own, where it could mean the onion endpoint or a wallet's address

**Address segment**:
The part of a TOON app's ILP addresses that no other TOON app has: taken from its connector's identity key, never chosen by the operator. Every ILP address the connector answers to sits under `g.toon.<address segment>`.
_Avoid_: Node name, node id, namespace

**Write edge**:
What a relay says about where a write to it is paid: the ILP address the write is sent to, the seal key of the connector that terminates it, and the price. A relay names its own write edge in its information document. The connector URL beside it is a location hint, not something the write depends on.
_Avoid_: Paid edge, payment endpoint

**Hidden service**:
The mode in which a connector is reachable only at an address inside the Anyone overlay, which public DNS cannot resolve. It is the default for a new TOON app.
_Avoid_: Onion service, Tor service, dark node

**Onion endpoint**:
The `.anyone` address a hidden-service connector publishes.
_Avoid_: Hidden-service URL, `.onion` address

**Clearnet**:
Reachable at an ordinary public hostname. Never the default; the operator asks for it explicitly.

### Finding out what it costs

**Terms**:
What a connector answers a packet that came without payment: its price for that destination, how to pay, and what to send the route where its operator declared it. Terms come from one connector and say nothing of the connectors on the way to it.
_Avoid_: Quote, offer

**Probe**:
A packet sent to learn what a path costs. It is expected to be rejected, and the reject states the path's cost: the fee of every connector that forwards it, and the terminating route's charge for the request sent. A probe can cost up to the amount it carries; by default it carries nothing and pays nothing. A probe that stopped at a connector it could not pay states a partial cost, the amount to carry to get past that connector. A probe whose amount covers the path's cost is delivered and paid for, as a send is.
_Avoid_: Quote, dry run; using "probe" for asking one connector its terms
