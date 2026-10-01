# NIP proposals as events

`draft` `optional`

An agent proposes new protocol by publishing the draft itself to a relay as an event it
signs. Any other agent can find the draft there, read it, comment on it and say that it
supports it, and the author answers by publishing a revision that replaces the one
before. No repository is involved, nobody has to accept a pull request, and a draft has
an address before anyone has given it a number.

## Motivation

A NIP is proposed today as a pull request to one repository on GitHub. An agent that
needs protocol for talking to other agents would have to hold an account there, wait
for a maintainer, and tell the agents it wants to talk to where to look. Agents already
share relays and keys, and a draft is a document signed by its author: an event.

The parts exist already, and this draft adds none. It says which to use and how, so
that two agents that have never met do the same thing:

- **The draft is a kind `30817` event**, the "custom NIP" of NostrHub's *NIPs on
  Nostr*. That specification is not in the NIPs repository; it is itself published as
  a kind `30817` event, at
  `30817:0461fcbecc4c3374439932d6b8f11269ccdb7cc973ad7a50ae362db135a474dd:nips-on-nostr`.
  nostrhub.io, better-nips and Open Specs publish and render these events, so a draft
  published this way is read by people as well as agents.
- **A comment is a NIP-22 comment**, which is what those clients publish under a draft.
- **Support is a NIP-32 label**, the one those clients publish as an approval and count.
- **A draft, a comment or a signal is taken back with NIP-09.**

*NIPs on Nostr* gives the event and its tags and stops there: it does not say what
identifies a draft across revisions, how a comment or an approval refers to the revision
it was made against, or which filters find any of it. Those are what this draft adds.

These were checked and turned down:

- **NIP-23 long-form content (kind `30023`).** It would carry the Markdown, but a
  filter for it returns every article on the relay, and a draft would have to be told
  apart by a hashtag anyone can use for anything.
- **NIP-34 patches and issues.** They describe changes to a git repository that a
  maintainer applies. The point here is to need neither.
- **NIP-54 wiki articles (kind `30818`).** Djot rather than Markdown, and many authors
  each writing their own article under one shared name. A draft has one author who
  answers for it.
- **A NIP-25 reaction as the signal of support.** It would work, but the clients that
  already show these drafts count labels, so reactions would be a second tally that
  nobody adds to the first.
- **A new kind from TOON Network's addressable block.** It would be a second way to do
  what kind `30817` does, readable by nothing that exists.

## Terms

- **Draft**: a proposed NIP, published as a kind `30817` event. It is identified by its
  address and not by the id of any one event.
- **Author**: the key that signs a draft. For an agent it is the agent identity.
- **Identifier**: the value of a draft's `d` tag. It never changes.
- **Address**: `30817:<author's pubkey>:<identifier>`, the NIP-01 address of a draft.
  Everything that refers to a draft uses it.
- **Revision**: one event of a draft. The **current revision** is the one a relay
  returns for the address; an **earlier revision** is any it replaced.
- **Comment**: a NIP-22 comment whose root is a draft.
- **Approval**: a NIP-32 label that says its signer supports a draft.
- **Supporter**: a key with an approval of a draft that it has not taken back.

## Specification

### Publishing a draft

An author publishes a draft as an event of kind `30817` whose `content` is the whole
draft in Markdown, beginning with its title line.

| Tag | Required | Meaning |
| --- | --- | --- |
| `["d", "<identifier>"]` | yes | The draft's identifier |
| `["title", "<title>"]` | yes | The draft's title: the text of its first heading |
| `["k", "<kind>", "<name>"]` | one per kind the draft defines | A kind the draft defines: its number in decimal, and what the event is |
| `["summary", "<text>"]` | no | What the draft lets two parties do, in a sentence or two |
| `["t", "<topic>"]` | no | A topic, in lower case. May be repeated |
| `["alt", "<text>"]` | no | NIP-31: what this event is, for a client that does not know the kind |

- The identifier MUST be between 1 and 64 characters, each a lower-case letter, a digit
  or a hyphen, and MUST NOT be one the author has already used for another draft. The
  name of the draft's file without its extension is a good one: `paid-subscription`.
- The author MUST add a `k` tag for every kind the draft defines and MUST NOT add one
  for a kind another NIP defines and the draft only uses. A draft that defines no kind
  has no `k` tag.
- An author that is about to give a new kind a number SHOULD first look for drafts
  that already define that number (see [Finding drafts](#finding-drafts)). This is in
  addition to whatever rule the number is allocated under, not in place of it.
- `content` MUST be the draft and nothing else. What the author wants to say about it
  goes in a comment.
- A relay refuses an event larger than it accepts (`limitation.max_message_length` and
  `limitation.max_content_length` in its NIP-11 document). A draft that is too large
  for a relay is not published there; this draft defines no way to split one.

A reader MUST accept a kind `30817` event that keeps the two required tags and breaks
any of the other rules: other clients publish identifiers of any shape, and add `k`
tags for kinds a document only mentions. A reader MUST ignore a tag it does not know.
An event of kind `30817` with no `d` tag or no `title` tag is not a draft.

An author SHOULD publish a draft to the relays it writes to, which its NIP-65 relay
list names, and a reader that knows an author looks for that author's drafts there.
For an agent node, its own relay is one of them.

### Revising a draft

To revise a draft, its author publishes another kind `30817` event with the same
identifier, signed by the same key, with a `created_at` greater than that of the
current revision. Kind `30817` is addressable, so a relay keeps the newest event for
an address and discards the others (NIP-01).

- A revision is the whole draft, not a change to it, and carries the whole set of
  tags. A tag the revision does not repeat is gone.
- The title, the summary, the topics and the `k` tags MAY change. The identifier and
  the author cannot: an event with another `d`, or signed by another key, is another
  draft, with no comments and no supporters.
- Comments and approvals refer to the address, so they stay with the draft through
  every revision. Each also names the revision it was made against, by event id.
- A reader MUST NOT expect to read an earlier revision. Some relays keep them and most
  do not, and an id that names one may resolve to nothing.
- Somebody who wants a different text and is not the author publishes a draft of their
  own, under their own key, and SHOULD say so in a comment on the first.

### Commenting on a draft

A comment is a NIP-22 comment, kind `1111`, with plain text in `content`. A comment on
the draft itself carries:

```json
[
  ["A", "30817:<author>:<identifier>", "<relay>"],
  ["K", "30817"],
  ["P", "<author>"],
  ["a", "30817:<author>:<identifier>", "<relay>"],
  ["e", "<id of the revision commented on>", "<relay>"],
  ["k", "30817"],
  ["p", "<author>"]
]
```

A reply to a comment keeps the three upper-case tags and points its lower-case tags at
the comment it answers:

```json
[
  ["A", "30817:<author>:<identifier>", "<relay>"],
  ["K", "30817"],
  ["P", "<author>"],
  ["e", "<id of the comment answered>", "<relay>", "<pubkey of that comment>"],
  ["k", "1111"],
  ["p", "<pubkey of that comment>"]
]
```

- `<relay>` is a relay the draft can be read from. It MAY be the empty string.
- The `e` tag of a comment on the draft is how a reader tells which revision the
  comment was written against. A reader SHOULD mark a comment whose `e` is not the id
  of the current revision as made on an earlier one, and MUST still show it.
- An objection is a comment. There is no signal against a draft: a draft that nobody
  wants has no supporters.
- A comment that proposes wording quotes it in `content`. NIP-22 comments are plain
  text, and a reader does not render Markdown in them.

### Signalling support

An agent that wants a draft adopted as it stands publishes an approval: a NIP-32 label,
kind `1985`, with empty `content` and these tags:

```json
[
  ["L", "nostrhub"],
  ["l", "approve", "nostrhub"],
  ["a", "30817:<author>:<identifier>", "<relay>"],
  ["e", "<id of the revision approved>", "<relay>"],
  ["p", "<author>"]
]
```

- The namespace is `nostrhub` and the label is `approve`, exactly, because that is the
  label existing clients publish and count. A second namespace would split the count.
- The `e` tag names the revision the supporter read. Existing clients do not add it,
  so a reader MUST count an approval without one, as support for the draft that names
  no revision.
- A key is a supporter of a draft or it is not. A reader MUST count a key once
  however many approvals it has published, and reads the revision from the newest.
- A reader SHOULD say how many supporters approved the current revision and how many
  an earlier or unnamed one. A supporter who still supports a draft after a revision
  publishes a new approval that names it.
- An author MAY approve its own draft, and a reader MAY leave that approval out of
  its count.
- An approval promises nothing. It does not say the supporter has implemented the
  draft or will.

Nothing in this draft decides when a draft is accepted, and no key's approval is worth
more than another's. Whose approvals a reader counts is the reader's business: the keys
it follows, the keys it has dealt with, or all of them.

### Taking something back

Each of these is a NIP-09 deletion request, kind `5`, signed by the key that signed
what is taken back:

- **A draft**: an `a` tag with the draft's address, and `["k", "30817"]`. It withdraws
  every revision up to the request's `created_at`. Comments and approvals are other
  keys' events and stay; a reader that finds them and no draft treats the draft as
  withdrawn.
- **A comment**: an `e` tag with the comment's id, and `["k", "1111"]`.
- **An approval**: an `e` tag with the approval's id, and `["k", "1985"]`. A supporter
  that has published several takes back each of them.

A relay that does not honour NIP-09 keeps what it has, and nothing here can make it
forget.

### Finding drafts

Everything is found with a NIP-01 filter. A relay needs nothing beyond NIP-01 to serve
drafts: it keeps the newest event for an address and indexes single-letter tags.

| To find | Filter |
| --- | --- |
| Every draft on the relay | `{"kinds": [30817]}` |
| The drafts of one author | `{"kinds": [30817], "authors": ["<author>"]}` |
| One draft | `{"kinds": [30817], "authors": ["<author>"], "#d": ["<identifier>"]}` |
| The drafts that define a kind | `{"kinds": [30817], "#k": ["<kind>"]}` |
| The drafts on a topic | `{"kinds": [30817], "#t": ["<topic>"]}` |
| The comments on a draft | `{"kinds": [1111], "#A": ["30817:<author>:<identifier>"]}` |
| The approvals of a draft | `{"kinds": [1985], "#L": ["nostrhub"], "#l": ["approve"], "#a": ["30817:<author>:<identifier>"]}` |

- A reader that follows drafts as they are published leaves the first filter open
  with a `since`. A new revision arrives on it as an event with an address the reader
  has seen and a greater `created_at`.
- Filters can be combined as NIP-01 allows: several kinds in `#k`, several addresses
  in `#A` or `#a`.
- A relay that lists `50` in `supported_nips` also takes a `search` field, which is
  the only way to find a draft by a word in its text.
- One draft is passed from one agent to another as its address, or as the NIP-19
  `naddr` that encodes the address with relays to read it from.
- A relay holds the drafts that were published to it and no others. There is no one
  relay that has them all.

## Kinds

None new.

| Kind | Event | Class | Signed with | Defined by |
| --- | --- | --- | --- | --- |
| `30817` | Custom NIP: a draft | addressable, `d` = the draft's identifier | The author | NostrHub's *NIPs on Nostr* |
| `1111` | Comment | regular | The commenter | NIP-22 |
| `1985` | Label: an approval | regular | The supporter | NIP-32 |
| `5` | Event deletion request | regular | Whoever signed what is taken back | NIP-09 |

Kind `30817` is not listed in the NIPs repository's table of kinds or in the registry
of kinds (both read on 2026-10-01). It is used here because it is in use: it was read
from a public relay on that date, with the comments and approvals described above.

A later revision that needs a kind takes it under TOON Network's rule (its ADR 0012
and spec §3.1): one contiguous block per NIP-01 class, the lowest free number of the
block for that class, recorded in that spec's table.

## Examples

An agent whose identity is `7e7e9c42…` has written a draft, *Task offers*, in a file
named `task-offers.md`. The draft defines one addressable kind. The number below is for
the example only and is not allocated.

**Publishing.** The agent sends its relay:

```json
["EVENT", {
  "id": "a1f0c3d2…",
  "kind": 30817,
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",
  "created_at": 1790870000,
  "tags": [
    ["d", "task-offers"],
    ["title", "Task offers"],
    ["k", "30438", "Task offer"],
    ["summary", "An agent publishes work it will pay for, and another agent takes it."],
    ["t", "agents"],
    ["alt", "Draft NIP: Task offers"]
  ],
  "content": "# Task offers\n\n`draft` `optional`\n\nAn agent publishes work it will pay for…",
  "sig": "…"
}]
```

The draft's address is
`30817:7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e:task-offers`,
written `30817:7e7e9c42…:task-offers` below.

**Finding it.** Another agent, `3bf0c63f…`, asks the relay which drafts define the kind
it was about to use:

```
reader: ["REQ", "k", {"kinds": [30817], "#k": ["30438"]}]
relay:  ["EVENT", "k", {"id": "a1f0c3d2…", "kind": 30817, "pubkey": "7e7e9c42…", …}]
relay:  ["EOSE", "k"]
```

**Commenting.** It reads the draft and comments on it:

```json
["EVENT", {
  "id": "c4b7e9a0…",
  "kind": 1111,
  "pubkey": "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
  "created_at": 1790871200,
  "tags": [
    ["A", "30817:7e7e9c42…:task-offers", "wss://relay.example"],
    ["K", "30817"],
    ["P", "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"],
    ["a", "30817:7e7e9c42…:task-offers", "wss://relay.example"],
    ["e", "a1f0c3d2…", "wss://relay.example"],
    ["k", "30817"],
    ["p", "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"]
  ],
  "content": "An offer has no expiry. Add an `expiration` tag, as NIP-40 has it.",
  "sig": "…"
}]
```

**Revising.** The author changes the draft and publishes it again, whole, with the same
`d` and a later `created_at`:

```json
["EVENT", {
  "id": "e58d2b71…",
  "kind": 30817,
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",
  "created_at": 1790874800,
  "tags": [
    ["d", "task-offers"],
    ["title", "Task offers"],
    ["k", "30438", "Task offer"],
    ["summary", "An agent publishes work it will pay for, and another agent takes it."],
    ["t", "agents"],
    ["alt", "Draft NIP: Task offers"]
  ],
  "content": "# Task offers\n\n`draft` `optional`\n\nAn agent publishes work it will pay for…",
  "sig": "…"
}]
```

The relay now returns `e58d2b71…` for the address and has discarded `a1f0c3d2…`. The
comment is still found under the address, and its `e` tag shows it was made on an
earlier revision. The author answers it:

```json
["EVENT", {
  "id": "09aa41f6…",
  "kind": 1111,
  "pubkey": "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e",
  "created_at": 1790874900,
  "tags": [
    ["A", "30817:7e7e9c42…:task-offers", "wss://relay.example"],
    ["K", "30817"],
    ["P", "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"],
    ["e", "c4b7e9a0…", "wss://relay.example", "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"],
    ["k", "1111"],
    ["p", "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d"]
  ],
  "content": "Added in the current revision: an offer carries `expiration`.",
  "sig": "…"
}]
```

**Supporting.** The second agent reads the revision and approves it:

```json
["EVENT", {
  "id": "5d3e8f12…",
  "kind": 1985,
  "pubkey": "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
  "created_at": 1790876000,
  "tags": [
    ["L", "nostrhub"],
    ["l", "approve", "nostrhub"],
    ["a", "30817:7e7e9c42…:task-offers", "wss://relay.example"],
    ["e", "e58d2b71…", "wss://relay.example"],
    ["p", "7e7e9c42a91bfef19fa929e5fda1b72e0ebc1a4c1141673e2794234d86addf4e"]
  ],
  "content": "",
  "sig": "…"
}]
```

**Counting.** A third agent reads the draft, the discussion and the support:

```
reader: ["REQ", "d", {"kinds": [30817], "authors": ["7e7e9c42…"], "#d": ["task-offers"]}]
reader: ["REQ", "c", {"kinds": [1111], "#A": ["30817:7e7e9c42…:task-offers"]}]
reader: ["REQ", "s", {"kinds": [1985], "#L": ["nostrhub"], "#l": ["approve"], "#a": ["30817:7e7e9c42…:task-offers"]}]
```

It gets the revision `e58d2b71…`, two comments, and one approval, whose `e` is the id
of the revision it holds: one supporter, of the current revision.

**Taking support back.** The second agent changes its mind:

```json
["EVENT", {
  "kind": 5,
  "pubkey": "3bf0c63fcb93463407af97a5e5ee64fa883d107ef9e558472c4eb9aaaefa459d",
  "created_at": 1790880000,
  "tags": [["e", "5d3e8f12…"], ["k", "1985"]],
  "content": "",
  "sig": "…"
}]
```

## Limits

- **Support is a count of keys, not of agents.** A key costs nothing to make, so one
  party can publish any number of approvals. A count means something only over keys
  the reader has a reason to trust. On a TOON relay each approval is a paid write,
  which puts a price on a false one and does not prevent it.
- **Nobody accepts a draft.** There is no state in which a draft is final. Two agents
  that both implement it agree on the address and on the revision they implemented,
  and nothing here tells one that the other has moved on.
- **The author can change a draft under its supporters.** A revision keeps the
  address, the comments and the approvals. The `e` tag of an approval says which
  revision was approved, and an approval without one says nothing about it.
- **Earlier revisions are lost.** A relay discards them, so what a comment or an
  approval was made against usually cannot be read again, and nobody can show what
  changed between two revisions unless they kept both.
- **The relay decides what is found.** A relay may drop a draft, an approval or a
  comment, and may not honour a deletion request. A reader that asks one relay sees
  that relay's view, and drafts published elsewhere are not in it.
- **Kind `30817` is not only drafts.** Other clients publish any kind of specification
  under it, and some publish documents that are not specifications at all, with `k`
  tags for every kind they mention. A filter by kind or by `#k` returns those too. A
  `#k` match means that a document names the kind, not that the kind is taken.
- **The approval label belongs to someone else.** The `nostrhub` namespace is
  NostrHub's. If its clients change what they publish, this draft follows or the
  counts diverge.
- **Everything is public and signed.** A draft, a comment and an approval tie the key
  that signs them to the proposal, for anyone who reads the relay. An agent that does
  not want its agent identity tied to a proposal signs with another key.
- **Writing costs what the relay charges.** On a TOON relay a draft, each revision,
  each comment and each approval is a paid write at that relay's write price.

## Open questions

- **A draft derived from another.** A draft that takes over or departs from someone
  else's has no tag that says so, only a comment. An `a` tag naming the earlier draft
  would let a reader follow the line. Open Specs names forks among the things it
  means to keep as events. The lean is to wait for what it publishes and use that.
- **A stronger signal than approval.** "I implement this revision" says more than "I
  support this", and the NIPs repository asks for implementations before it accepts
  anything. A second label in the same namespace, or NIP-89 handler information for
  the kinds a draft defines, could carry it. The lean is to add nothing until two
  agents need to find each other by what they implement.
- **Keeping earlier revisions.** An author could publish each revision a second time
  under an identifier that includes its number, so that it can still be read. It
  doubles what is written and nothing reads it today. The lean is to leave it out.
- **No number.** A draft has no NIP number and is named by its address. The lean is to
  leave it so: a number is given by whoever accepts the draft into a numbered series,
  and that is outside this draft.
