# Concepts

The words below have one meaning each, everywhere in `toon`, its output and these guides.
[`CONTEXT.md`](../../CONTEXT.md) is the full glossary.

## What runs

**Agent node.** Everything one operator runs with `toon` on one machine: one wallet and one
or more TOON apps. `toon init` creates it, `toon up` starts it.

**TOON app.** One connector together with the apps behind it. The first one, which
`toon init` creates, is the *relay TOON app*: its only app is the relay.

**Connector.** The paid reverse proxy at the front of a TOON app. It accepts a packet,
checks that it is paid, and either delivers the request inside it to one of its apps or
forwards it to a peer. It has its own identity key and its own settlement keys.

**App.** A plain HTTP service behind a connector. It is *payment-oblivious*: it receives an
ordinary HTTP request and returns an ordinary answer, and never sees a packet, a key or a
payment. The relay is an app. An app on its own is never called a TOON app.

**Relay.** The Nostr relay app every agent node has, the one `toon init` creates. Writing to
it is paid through its connector. An agent node has exactly one; `toon add` and
`toon create` refuse the relay's image.

**Supervisor.** The one process per machine that runs the agent node. It starts each
connector as a child process, starts each app, restarts a connector that exits, and keeps
paid subscriptions flowing into the relay.

**Operator.** Whoever runs the agent node: you, or an agent acting through `toon`.

## How packets find their way

**ILP address.** Where a packet is sent: `g.toon`, then the TOON app's address segment,
then the app. `g.toon.3fa29c01b2d4e5f6.search` is the app `search` behind one connector.
It names an app, not a machine: where the connector is dialled is a separate thing, its
onion endpoint or its clearnet hostname.

**Address segment.** The part of a TOON app's addresses that no other TOON app has. It is
taken from the connector's identity key, so you never choose it and it cannot collide.

**Route.** A connector's rule for a prefix of ILP addresses: either *terminate* it at one of
its apps, at a price, or *forward* it to a peering. `toon add` writes the first kind,
`toon route add` the second.

## How money moves

**Wallet.** The keys and funds of the agent node. `toon` generates them from one mnemonic
and keeps them in a keystore sealed by your passphrase.

**Channel.** Collateral a connector locks on chain toward another connector. Packets are
paid by signing vouchers against it, so a payment costs no transaction; only opening,
funding, withdrawing and landing touch the chain.

**Peering.** A connector's standing arrangement to forward packets to another connector,
paid on a channel it opens toward it. A peering goes one way. The other connector sends
packets back only if its operator creates a peering in return.

**Spending limit.** The most the wallet pays out per command and per UTC day. Every command
that moves money is checked against it before anything is sent.

**Agent identity.** The Nostr key your agent signs events with. It belongs to the wallet but
can never pay: losing control of it costs a reputation, not funds (ADR 0004).

## How it is reached

**Hidden service.** The default. A connector listens on loopback only and is reachable at an
**onion endpoint**, a `.anyone` address on the Anyone overlay, which public DNS cannot
resolve. Everything it sends goes through the overlay too. It hides *where* the machine is,
not *whom it pays*: payments are on a public chain.

**Clearnet.** An ordinary public hostname, for an operator who asks for it with
`--clearnet <hostname>` and brings their own certificate and reverse proxy.

## Why it is shaped like this

- **Apps stay payment-oblivious** so any HTTP service can be sold without changing it, and
  so payment code lives in one audited place, the connector.
- **The CLI embeds the connector** (ADR 0001): one binary, one version, no second thing to
  install or keep in step.
- **Several apps may share a connector** (ADR 0002): one operator trusts themselves, so a
  second connector is only worth its extra keys, channels and peerings when an app needs its
  own identity, prices or peerings.
- **Hidden by default, failing closed** (ADR 0003): an agent usually runs on a machine with
  no public hostname and no reason to reveal where it is.
- **Addresses come from keys** (ADR 0006): nobody has to coordinate names.

Next: [Your first agent node](first-agent-node.md).
