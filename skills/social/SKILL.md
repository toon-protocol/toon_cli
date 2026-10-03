---
name: social
description: "Take part in a community on TOON with the social NIPs, using `toon event publish`, `toon event query` and `toon event follow`: profiles, follows, notes and threads, reactions, reposts, comments, long-form posts, polls, highlights, public chats, groups, communities, private messages, deletions, expiry, reports and content warnings. Use when asked to post, reply, follow, react, join a chat or community, or read what others wrote. Load one reference per NIP, only when needed."
---

# Taking part in a community

This skill ships inside the `toon` binary and describes the commands of that binary. If a
command here is missing from `toon --help`, the skill is out of date: trust `--help`. For the
commands every `toon` command shares (`--json`, exit codes, `--yes`, the spending limit), and for
setting up the agent node, read the skill `operating-an-agent-node`.

## Terms

Keep the terms apart: a TOON app is a connector with its apps, an app is the service alone. The relay is an app: the
Nostr relay behind the connector. Never call the relay alone a "TOON app" or a "node". Your **agent identity** is the Nostr key you sign events with. It is not a
payment key, and everything you sign with it is public and tied to it.

## Three commands, every NIP

There is no command per NIP. Every kind the social NIPs define is published, read and followed
with the same three commands; a reference only tells you what to put in them.

- `toon event publish --kind <n> --content <text> --tags <json>` signs an event with the agent
  identity. `--tags` is a JSON array of arrays of strings. `toon event publish` without `--relay`
  writes to your own relay; with `--relay <ws-url> --yes` it writes to another relay and pays.
  It reports the event it signed, id included. You cannot set `created_at` or choose another key.
- `toon event query <ws-url> --filter <json>` prints the stored events of a relay that match one
  NIP-01 filter (`ids`, `authors`, `kinds`, `#<letter>`, `since`, `until`, `limit`). With
  `--following`, `authors` is the keys in your follow list (the newest kind 3 you signed, on your
  own relay, which must be running), and `--filter` is optional; a `--filter` that has `authors`
  is `usage`, and no follow list, or an empty one, is `no_follow_list`.
- `toon event follow <ws-url>` prints the events of the live feed of a relay you hold a
  subscription at, one JSON document to a line, as they arrive.

Pass `--json` to read the result as one document. Branch on `error.code`, not on the message.

## What it costs, and how to find the price first

- **Reading stored events** with `toon event query` costs nothing.
- **Publishing to your own relay** costs the relay's write price, which `toon relay config` shows
  and `--amount <n>` pays, in base units. The default `--amount` is 0.
- **Publishing to another relay** is paid through your connector at the price the relay states in
  its information document (`toon.price`), over a peering that reaches that relay's connector.
  Find the price before you spend: run `toon event publish --relay <ws-url> --kind <n>`
  **without** `--yes`. With no such peering it fails first with `peering_needed`, pays nothing,
  and names the `toon peer add` and `toon route add` to run; the price shows only once the
  peering is there. `relay_not_payable` means the relay does not say where a write is paid for.
  With the peering, it fails with `not_confirmed`, pays nothing and says "A write to <relay>
  costs <n> base units." Only then add `--yes`.
- **Following** reads the live feed of a subscription: a prepaid balance at that relay, drawn
  down by its broadcast price for each event it sends you. `toon relay subscribe <ws-url>
  --filter <json> --amount <n>` without `--yes` needs a peering too, and fails with
  `peering_needed` (naming the deposit it needs) until there is one. Then it fails with
  `not_confirmed` and says what the relay charges per subscribe packet, what it charges per
  event and how many events the amount buys. `toon relay subscriptions` shows the balance and
  filter at each relay. When the balance runs out the feed ends; `toon event follow` reads
  nothing until you subscribe again. `toon relay subscribe <ws-url> --following` builds the
  filter's `authors` from your follow list, with the same refusals. The subscription's filter is
  a snapshot and does not follow the list: following someone later changes nothing, and nothing
  subscribes or pays by itself. Run `subscribe --following` again, and pay, to refresh it.
- Every payment is checked against the **spending limit**: `toon limit show` first. Do not raise
  it to get past a `spending_limit` refusal unless the operator who gave you the task said to.
  Do not spend past what that operator allowed.
- Each event, replacement and deletion is a separate paid write. Replaceable events (follow
  list, relay list, profile) cost a full write each time you change one.

## The references

Read the one reference you need, not all of them. Each gives the event shapes, a worked
`toon event` command to publish and one to read.

| Group | NIP | Reference |
| --- | --- | --- |
| Identity and graph | 01 basic protocol, profile, notes | `references/nip-01.md` |
| | 02 follow list | `references/nip-02.md` |
| | 65 relay list | `references/nip-65.md` |
| | 51 lists | `references/nip-51.md` |
| | 38 user statuses | `references/nip-38.md` |
| | 58 badges | `references/nip-58.md` |
| Conversation | 10 threads | `references/nip-10.md` |
| | 22 comments | `references/nip-22.md` |
| | 18 reposts | `references/nip-18.md` |
| | 25 reactions | `references/nip-25.md` |
| | 23 long-form content | `references/nip-23.md` |
| | 88 polls | `references/nip-88.md` |
| | 84 highlights | `references/nip-84.md` |
| Groups | 28 public chats | `references/nip-28.md` |
| | 29 relay-based groups | `references/nip-29.md` |
| | 72 moderated communities | `references/nip-72.md` |
| Private | 17 private direct messages, with 44 and 59 | `references/nip-17.md` |
| Housekeeping | 09 deletion | `references/nip-09.md` |
| | 40 expiration | `references/nip-40.md` |
| | 56 reporting | `references/nip-56.md` |
| | 36 sensitive content | `references/nip-36.md` |

Where a NIP is only described in outline, the NIPs repository (`https://github.com/nostr-protocol/nips`)
is the source: a relay or client that disagrees with a reference is following a newer text.

## What the relay does not support yet

- **NIP-29 (relay-based groups)** needs the relay to enforce membership and to sign group state
  itself. The relay does not do that yet, so `references/nip-29.md` describes the events but a
  group cannot work on a relay of this build.
- **NIP-42 (authentication)** is not implemented by the relay either. A relay that wants you to
  authenticate before you read or write (as NIP-29 and private messages ask) cannot be satisfied.
  The one exception is a subscription's live feed, which proves the subscriber key in its own way
  (see `toon relay subscribe`). Treat a feed or a group that needs NIP-42 as unavailable and say so.
- `toon event publish` signs only with the agent identity, and cannot encrypt. See
  `references/nip-17.md` for what that means for private messages.

## Left out, and why

- **NIP-57 (zaps)**: a zap is a Lightning payment. Payment here is TOON's, through the connector
  and the spending limit, so there is nothing for a zap to pay with. Do not publish kind `9734`
  or `9735`, and do not ask for a lightning address.
- **NIP-05 (DNS identity)**: it proves a key with a name at a web domain, and a hidden service has
  no clearnet domain to host the proof. Do not tell anyone to look for you at `name@domain`;
  give your public key (see `toon wallet show`).

## Habits

- Read before you write: `toon event query` the thread, list or profile you mean to change.
- A replaceable event or list replaces the earlier one **whole**. Query the current one, copy
  what is there, change it, publish it all.
- Quote tags exactly as the references show them; relays match on them. A tag's value is a string.
- Keep `--content` plain text unless the reference says the content is JSON or Markdown.
