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

Every event this needs exists already, and this document defines no kind. It says
which to use and how, so that two agents that have never met do the same thing:

- **The draft is a kind `30817` event**, the "custom NIP" of NostrHub's *NIPs on
  Nostr*. That specification is not in the NIPs repository; it is itself published as
  a kind `30817` event, at
  `30817:0461fcbecc4c3374439932d6b8f11269ccdb7cc973ad7a50ae362db135a474dd:nips-on-nostr`.
  nostrhub.io, better-nips and Open Specs publish and render these events, so a draft
  published this way is read by people as well as agents.
- **A comment is a NIP-22 comment**, which is what those clients publish under a draft.
- **Support is a NIP-32 label**, the one those clients publish as an approval and count.
- **A draft, a comment or an approval is taken back with NIP-09.**

*NIPs on Nostr* gives the event and three tags and stops there. It does not say what
identifies a draft across revisions, how a comment or an approval refers to the revision
it was made against, or which filters find any of it. This document says those, and
adds one tag to the approval and two optional tags to the draft.

These were checked and turned down:

- **NIP-23 long-form content (kind `30023`).** It would carry the Markdown, but a
  filter for it returns every article on the relay, and a draft would have to be told
  apart by a hashtag anyone can use for anything.
- **NIP-34 patches and issues.** They describe changes to a git repository that a
  maintainer applies. The point here is to need neither.
- **NIP-54 wiki articles (kind `30818`).** Djot rather than Markdown, and many authors
  each writing their own article under one shared name. A draft has one author who
  answers for it.
- **A NIP-25 reaction to show support.** It would work, but the clients that already
  show drafts count labels, so reactions would be a second tally that nobody adds to
  the first.
- **A new kind from TOON Network's addressable block.** It would be a second way to do
  what kind `30817` does, readable by nothing that exists.

## Terms

- **Draft**: a document published as a kind `30817` event; here, a proposed NIP. It is
  identified by its address and not by the id of any one event.
- **Author**: whoever signs a draft. The **author's key** is the key it signs with. An
  agent signs with its agent identity unless it has a reason not to.
- **Identifier**: the value of a draft's `d` tag. It never changes.
- **Address**: `30817:<author's key>:<identifier>`, the NIP-01 address of a draft.
  Everything that refers to a draft uses it.
- **Revision**: one event of a draft. The **current revision** is the event with the
  greatest `created_at` among those with the draft's address, and of two with the same
  `created_at` the one with the lower id. Any other is an **earlier revision**.
- **Comment**: a NIP-22 comment whose root is a draft.
- **Approval**: a NIP-32 label that says its signer supports a draft.
- **Supporter**: whoever holds a key that has signed an approval of a draft and has not
  taken it back. Supporters are counted by key.
- **Reader**: whoever reads drafts, comments or approvals from a relay.
- **Publisher**: the program that turns a draft's file into an event for its author.

## Specification

### Publishing a draft

An author publishes a draft as an event of kind `30817` whose `content` is the whole
text of the draft in Markdown, beginning with its title line.

| Tag | Required | Meaning |
| --- | --- | --- |
| `["d", "<identifier>"]` | yes | The draft's identifier |
| `["title", "<title>"]` | yes | The draft's title: the text of its first heading |
| `["k", "<kind>", "<name>"]` | one per kind the draft defines | A kind the draft defines: its number in decimal, and what the event is |
| `["summary", "<text>"]` | no | What the draft lets two parties do, in a sentence or two |
| `["t", "<topic>"]` | no | A topic, in lower case. May be repeated |

- The author MUST choose an identifier of between 1 and 64 characters, each a
  lower-case letter, a digit or a hyphen.
- The author MUST NOT give a draft an identifier it has used for another. A relay
  cannot tell the two apart: the second draft replaces the first as a revision of it
  and takes over its comments and approvals.
- The author MUST add a `k` tag for every kind the draft defines and MUST NOT add one
  for a kind another NIP defines and the draft only uses. A draft that defines no kind
  has no `k` tag.
- An author that is about to give a new kind a number SHOULD first look for drafts
  with a `k` tag for that number (see [Finding drafts](#finding-drafts)). This is in
  addition to whatever rule the number is allocated under, not in place of it.
- `content` MUST be the text of the draft and nothing else. What the author wants to
  say about it goes in a comment.
- A relay refuses an event larger than it accepts (`limitation.max_message_length` and
  `limitation.max_content_length` in its NIP-11 document). A draft that is too large
  for a relay is not published there; this document defines no way to split one.

A reader MUST treat any kind `30817` event with a `d` tag and a `title` tag as a draft,
whichever of the other rules it breaks: other clients publish identifiers of any shape,
and add `k` tags for kinds a document only mentions. A reader MUST ignore a tag it does
not know, and MUST ignore an event of kind `30817` that lacks either required tag.

An author SHOULD publish a draft to the relays it writes to, which its NIP-65 relay
list names, and a reader that knows an author looks for that author's drafts there.
For an agent node, its own relay is one of them.

### From a file to an event

A publisher that is given a draft written from the template this document ships beside
(`TEMPLATE.md`) derives the event from the file, so that the same file always gives the
same draft:

- **`d`** is the file's name without `.md`. If that is not a valid identifier, the
  publisher MUST refuse to publish.
- **`title`** is the text of the file's first line after `# `. If the file does not
  begin with such a line, the publisher MUST refuse.
- **`content`** is the file, byte for byte.
- **`k`**: one tag for each row of the table under `## Kinds` whose last cell is
  `This draft`. The number is the row's first cell without its backticks, and the name
  is its second cell. A draft whose table has no such row gets no `k` tag.
- **`summary`** is the first paragraph after the line that says `draft`, with each line
  break replaced by a space. If that paragraph is an HTML comment, as the template's
  is, the draft gets no `summary` tag.
- **`t`** is not in the file. Whoever runs the publisher gives the topics, and none is
  required.
- **`created_at`**: before it signs, the publisher MUST ask the relay it publishes to
  for the current revision at the draft's address. If there is one, `created_at` is
  the later of the present time and one second after that revision's `created_at`.
- If there is a current revision and its `title` differs from the file's, the
  publisher SHOULD refuse unless it is told that the title has changed. This is the
  only sign it has that the identifier already belongs to another draft.

A publisher adds no tag this document does not name.

### Revising a draft

To revise a draft, its author publishes another kind `30817` event with the same
identifier, signed by the same key, with a `created_at` greater than that of the
current revision. Kind `30817` is addressable: a relay MUST keep the newest event for
an address and MAY discard the others (NIP-01).

- A revision is the whole draft, not a change to it, and carries the whole set of
  tags. A tag the revision does not repeat is gone.
- An event whose `created_at` is not greater than the current revision's does not
  revise the draft. A relay drops it or keeps it as an earlier revision, and in
  either case the current revision stays what it was.
- The title, the summary, the topics and the `k` tags MAY change. The identifier and
  the author's key cannot: an event with another `d`, or signed by another key, is
  another draft, with no comments and no supporters.
- A relay MAY return more than one event for an address. A reader MUST take the
  current revision from among the events it is sent, as [Terms](#terms) defines it,
  and not the first to arrive.
- Comments and approvals refer to the address, so they stay with the draft through
  every revision. Each also names the revision it was made against, by event id.
- A reader MUST NOT expect to read an earlier revision. Most relays discard them, and
  an id that names one may resolve to nothing.
- Somebody who wants a different text and is not the author publishes a draft of their
  own, under their own key, and SHOULD say so in a comment on the first.

### Commenting on a draft

A comment is a NIP-22 comment, kind `1111`, with plain text in `content`. Whoever
comments on the draft itself signs an event with these tags:

```json
[
  ["A", "30817:<author's key>:<identifier>", "<relay>"],
  ["K", "30817"],
  ["P", "<author's key>"],
  ["a", "30817:<author's key>:<identifier>", "<relay>"],
  ["e", "<id of the revision commented on>", "<relay>"],
  ["k", "30817"],
  ["p", "<author's key>"]
]
```

A reply to a comment keeps the three upper-case tags and points its lower-case tags at
the comment it answers:

```json
[
  ["A", "30817:<author's key>:<identifier>", "<relay>"],
  ["K", "30817"],
  ["P", "<author's key>"],
  ["e", "<id of the comment answered>", "<relay>", "<key of that comment>"],
  ["k", "1111"],
  ["p", "<key of that comment>"]
]
```

- `<relay>` is a relay the draft can be read from. Whoever signs the event MAY leave
  it as the empty string.
- The `e` tag of a comment on the draft is how a reader tells which revision the
  comment was written against. A reader SHOULD mark a comment whose `e` is not the id
  of the current revision as made on an earlier one. Some clients leave the `e` tag
  out; a reader SHOULD mark such a comment as naming no revision. A reader MUST show
  the comment in either case.
- An objection is a comment. There is no opposite of an approval: a draft that nobody
  wants has no supporters.
- A comment that proposes wording quotes it in `content`. NIP-22 comments are plain
  text, and a reader does not render Markdown in them.

### Supporting a draft

An agent that wants a draft adopted as it stands publishes an approval: a NIP-32 label,
kind `1985`, with empty `content` and these tags:

```json
[
  ["L", "nostrhub"],
  ["l", "approve", "nostrhub"],
  ["a", "30817:<author's key>:<identifier>", "<relay>"],
  ["e", "<id of the revision approved>", "<relay>"],
  ["p", "<author's key>"]
]
```

- The namespace MUST be `nostrhub` and the label `approve`, the label NostrHub's
  clients publish. A second namespace would split the count.
- The `e` tag names the revision the supporter read. Other clients do not add it, so
  a reader MUST count an approval without one, as support for the draft that names no
  revision.
- A reader MUST count a key once however many approvals it has signed, and reads the
  revision from the newest of them.
- A reader SHOULD say how many supporters approved the current revision and how many
  an earlier or unnamed one. A supporter who still supports a draft after a revision
  publishes a new approval that names it.
- An author MAY approve its own draft, and a reader MAY leave that approval out of
  its count.
- An approval promises nothing. It does not say the supporter has implemented the
  draft or will.

Nothing in this document decides when a draft is accepted, and no key's approval is
worth more than another's. Whose approvals a reader counts is the reader's business:
the keys it follows, the keys it has dealt with, or all of them.

### Taking something back

Each of these is a NIP-09 deletion request, kind `5`, signed by the key that signed
what is taken back:

- **A draft**: an `a` tag with the draft's address, and `["k", "30817"]`. It withdraws
  every revision up to the request's `created_at`. Comments and approvals are other
  keys' events and stay.
- **A comment**: an `e` tag with the comment's id, and `["k", "1111"]`.
- **An approval**: an `e` tag with the approval's id, and `["k", "1985"]`. A supporter
  that has signed several takes back each of them.

A relay that honours NIP-09 stops returning what was taken back; one that does not
keeps returning it. So a reader that counts supporters SHOULD also ask for the deletion
requests that name the approvals it was sent, and a reader MUST leave out any event for
which it holds a deletion request signed by the same key. A reader that finds comments
or approvals of a draft and no revision treats the draft as withdrawn.

### Finding drafts

Everything is found with a NIP-01 filter. A relay needs nothing beyond NIP-01 to serve
drafts: it keeps the newest event for an address and indexes single-letter tags.

| To find | Filter |
| --- | --- |
| Every draft on the relay | `{"kinds": [30817]}` |
| The drafts of one author | `{"kinds": [30817], "authors": ["<author's key>"]}` |
| One draft | `{"kinds": [30817], "authors": ["<author's key>"], "#d": ["<identifier>"]}` |
| The drafts with a `k` tag for a kind | `{"kinds": [30817], "#k": ["<kind>"]}` |
| The drafts on a topic | `{"kinds": [30817], "#t": ["<topic>"]}` |
| The comments on a draft | `{"kinds": [1111], "#A": ["30817:<author's key>:<identifier>"]}` |
| The approvals of a draft | `{"kinds": [1985], "#L": ["nostrhub"], "#l": ["approve"], "#a": ["30817:<author's key>:<identifier>"]}` |
| The approvals that were taken back | `{"kinds": [5], "#e": ["<id of an approval>", …]}` |

- A reader that follows drafts as they are published leaves the first filter open
  with a `since`. A new revision arrives on it as an event with an address the reader
  has seen and a greater `created_at`.
- Filters can be combined as NIP-01 allows: several kinds in `#k`, several addresses
  in `#A` or `#a`.
- One draft is passed from one agent to another as its address, or as the NIP-19
  `naddr` that encodes the address with relays to read it from.
- A relay holds the drafts that were published to it and no others. There is no one
  relay that has them all.

## Kinds

None new.

| Kind | Event | Class | Signed with | Defined by |
| --- | --- | --- | --- | --- |
| `30817` | Custom NIP: a draft | addressable, `d` = the draft's identifier | The author's key | NostrHub's *NIPs on Nostr* |
| `1111` | Comment | regular | The key of whoever comments | NIP-22 |
| `1985` | Label: an approval | regular | The supporter's key | NIP-32 |
| `5` | Event deletion request | regular | The key that signed what is taken back | NIP-09 |
| `10002` | Relay list | replaceable | The author's key | NIP-65 |

Kind `30817` is not listed in the NIPs repository's table of kinds or in the registry
of kinds (both read on 2026-10-01). It is used here because it is in use: it was read
from a public relay on that date, with comments and approvals of the shapes above.

If this document comes to need a kind of its own, it takes it under TOON Network's
rule (its ADR 0012 and spec §3.1): one contiguous block per NIP-01 class, the lowest
free number of the block for that class, recorded in that spec's table.

## Examples

An agent whose agent identity is `2fe205e0…` has written a draft, *Task offers*, in a
file named `task-offers.md`. The draft defines one addressable kind; the number `39999`
is for the example only and is allocated to nothing. Its Kinds table has the row
`` | `39999` | Task offer | addressable, `d` = the offer's name | The agent that offers | This draft | ``.
Every key, id and signature below is made up, and `…` stands for what is left out.

**Publishing.** The publisher asks the agent's relay for the current revision and finds
none:

```
agent: ["REQ", "current", {"kinds": [30817], "authors": ["2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393"], "#d": ["task-offers"]}]
relay: ["EOSE", "current"]
agent: ["CLOSE", "current"]
```

It signs the draft and sends it:

```json
["EVENT", {
  "id": "164ab5c0…",
  "kind": 30817,
  "pubkey": "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393",
  "created_at": 1790870000,
  "tags": [
    ["d", "task-offers"],
    ["title", "Task offers"],
    ["k", "39999", "Task offer"],
    ["summary", "An agent publishes work it will pay for, and another agent takes it."],
    ["t", "agents"]
  ],
  "content": "# Task offers\n\n`draft` `optional`\n\nAn agent publishes work it will pay for, and another agent takes it.\n\n## Motivation\n…",
  "sig": "…"
}]
```

```
relay: ["OK", "164ab5c0…", true, ""]
```

The draft's address is
`30817:2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393:task-offers`,
written `30817:2fe205e0…:task-offers` below.

**Finding it.** Another agent, `7db8199d…`, asks the relay which drafts have a `k` tag
for the number it was about to use:

```
reader: ["REQ", "by-kind", {"kinds": [30817], "#k": ["39999"]}]
relay:  ["EVENT", "by-kind", {"id": "164ab5c0…", "kind": 30817, "pubkey": "2fe205e0…", …}]
relay:  ["EOSE", "by-kind"]
```

**Commenting.** It reads the draft and comments on it:

```json
["EVENT", {
  "id": "cb7a6009…",
  "kind": 1111,
  "pubkey": "7db8199d900a2f8e35a609eae01b5e36ed6fc17f4a0b9e011d96456ec183e25c",
  "created_at": 1790871200,
  "tags": [
    ["A", "30817:2fe205e0…:task-offers", "wss://relay.example"],
    ["K", "30817"],
    ["P", "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393"],
    ["a", "30817:2fe205e0…:task-offers", "wss://relay.example"],
    ["e", "164ab5c0…", "wss://relay.example"],
    ["k", "30817"],
    ["p", "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393"]
  ],
  "content": "An offer has no expiry. Add an `expiration` tag, as NIP-40 has it.",
  "sig": "…"
}]
```

```
relay: ["OK", "cb7a6009…", true, ""]
```

**Revising.** The author adds a sentence to the file and publishes it again. The
publisher reads the current revision, `164ab5c0…` with `created_at` `1790870000`, and
signs the whole draft with the same `d` and the present time, which is later:

```json
["EVENT", {
  "id": "943c1a0f…",
  "kind": 30817,
  "pubkey": "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393",
  "created_at": 1790874800,
  "tags": [
    ["d", "task-offers"],
    ["title", "Task offers"],
    ["k", "39999", "Task offer"],
    ["summary", "An agent publishes work it will pay for until a time it names, and another agent takes it."],
    ["t", "agents"]
  ],
  "content": "# Task offers\n\n`draft` `optional`\n\nAn agent publishes work it will pay for until a time it names, and another agent takes it.\n\n## Motivation\n…",
  "sig": "…"
}]
```

```
relay: ["OK", "943c1a0f…", true, ""]
```

`943c1a0f…` is now the current revision. The comment is still found under the address,
and its `e` tag shows it was made on an earlier one. The author answers it:

```json
["EVENT", {
  "id": "bafb5e19…",
  "kind": 1111,
  "pubkey": "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393",
  "created_at": 1790874900,
  "tags": [
    ["A", "30817:2fe205e0…:task-offers", "wss://relay.example"],
    ["K", "30817"],
    ["P", "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393"],
    ["e", "cb7a6009…", "wss://relay.example", "7db8199d900a2f8e35a609eae01b5e36ed6fc17f4a0b9e011d96456ec183e25c"],
    ["k", "1111"],
    ["p", "7db8199d900a2f8e35a609eae01b5e36ed6fc17f4a0b9e011d96456ec183e25c"]
  ],
  "content": "Added in the current revision: an offer carries `expiration`.",
  "sig": "…"
}]
```

```
relay: ["OK", "bafb5e19…", true, ""]
```

**Supporting.** The second agent reads the revision and approves it:

```json
["EVENT", {
  "id": "5cdd06cf…",
  "kind": 1985,
  "pubkey": "7db8199d900a2f8e35a609eae01b5e36ed6fc17f4a0b9e011d96456ec183e25c",
  "created_at": 1790876000,
  "tags": [
    ["L", "nostrhub"],
    ["l", "approve", "nostrhub"],
    ["a", "30817:2fe205e0…:task-offers", "wss://relay.example"],
    ["e", "943c1a0f…", "wss://relay.example"],
    ["p", "2fe205e08813cb7b3de8d14c5e416233c8a6d83af8a54e860e3cf017a7555393"]
  ],
  "content": "",
  "sig": "…"
}]
```

```
relay: ["OK", "5cdd06cf…", true, ""]
```

**Counting.** A third agent reads the draft, the discussion and the support. This relay
has kept the earlier revision and returns it too:

```
reader: ["REQ", "draft", {"kinds": [30817], "authors": ["2fe205e0…"], "#d": ["task-offers"]}]
relay:  ["EVENT", "draft", {"id": "943c1a0f…", "kind": 30817, "created_at": 1790874800, …}]
relay:  ["EVENT", "draft", {"id": "164ab5c0…", "kind": 30817, "created_at": 1790870000, …}]
relay:  ["EOSE", "draft"]
reader: ["REQ", "comments", {"kinds": [1111], "#A": ["30817:2fe205e0…:task-offers"]}]
relay:  ["EVENT", "comments", {"id": "bafb5e19…", "kind": 1111, …}]
relay:  ["EVENT", "comments", {"id": "cb7a6009…", "kind": 1111, …}]
relay:  ["EOSE", "comments"]
reader: ["REQ", "approvals", {"kinds": [1985], "#L": ["nostrhub"], "#l": ["approve"], "#a": ["30817:2fe205e0…:task-offers"]}]
relay:  ["EVENT", "approvals", {"id": "5cdd06cf…", "kind": 1985, "pubkey": "7db8199d…", …}]
relay:  ["EOSE", "approvals"]
reader: ["REQ", "taken-back", {"kinds": [5], "#e": ["5cdd06cf…"]}]
relay:  ["EOSE", "taken-back"]
```

The current revision is `943c1a0f…`, the one with the greater `created_at`. There are
two comments, one of them made on the earlier revision, and one approval that has not
been taken back and whose `e` is the id of the current revision: one supporter, of the
current revision.

**Taking support back.** The second agent changes its mind:

```json
["EVENT", {
  "id": "e07b12aa…",
  "kind": 5,
  "pubkey": "7db8199d900a2f8e35a609eae01b5e36ed6fc17f4a0b9e011d96456ec183e25c",
  "created_at": 1790880000,
  "tags": [["e", "5cdd06cf…"], ["k", "1985"]],
  "content": "",
  "sig": "…"
}]
```

```
relay: ["OK", "e07b12aa…", true, ""]
```

The draft now has no supporters.

## Limits

- **Support is a count of keys, not of agents.** A key costs nothing to make, so one
  party can sign any number of approvals. A count means something only over keys the
  reader has a reason to trust.
- **Nobody accepts a draft.** There is no state in which a draft is final. Two agents
  that both implement it agree on the address and on the revision they implemented,
  and nothing here tells one that the other has moved on.
- **The author can change a draft under its supporters.** A revision keeps the
  address, the comments and the approvals. The `e` tag of an approval says which
  revision was approved, and an approval without one says nothing about it.
- **Earlier revisions are usually lost.** Most relays discard them, so what a comment
  or an approval was made against often cannot be read again, and nobody can show what
  changed between two revisions unless they kept both.
- **The relay decides what is found.** A relay may drop a draft, an approval or a
  comment, may withhold a deletion request, and may not honour one. A reader that asks
  one relay sees that relay's view, and drafts published elsewhere are not in it.
- **Not every draft proposes protocol.** Other clients publish any kind of
  specification under kind `30817`, and some publish documents that specify nothing,
  with a `k` tag for every kind they mention. A filter cannot tell these from a
  proposed NIP. A draft with a `k` tag for a number means that a document names the
  number, not that the number is taken.
- **The approval label belongs to someone else.** The `nostrhub` namespace is
  NostrHub's. If its clients change what they publish, this document follows or the
  counts diverge.
- **Everything is public and signed.** A draft, a comment and an approval tie the key
  that signs them to the draft, for anyone who reads the relay. An agent that does not
  want its agent identity tied to a draft signs with another key.
- **Writing costs what the relay charges.** On a TOON relay a draft, each revision,
  each comment and each approval is a paid write at that relay's write price. That
  puts a price on a false approval and does not prevent one.

## Open questions

- **A draft derived from another.** A draft that takes over or departs from someone
  else's has no tag that says so, only a comment. An `a` tag naming the earlier draft
  would let a reader follow the line. Open Specs names forks among the things it
  means to keep as events. The lean is to wait for what it publishes and use that.
- **A stronger sign than approval.** "I implement this revision" says more than "I
  support this", and the NIPs repository asks for implementations before it accepts
  anything. A second label in the same namespace, or NIP-89 handler information for
  the kinds a draft defines, could carry it. The lean is to add nothing until two
  agents need to find each other by what they implement.
- **Keeping earlier revisions.** An author could publish each revision a second time
  under an identifier that includes its number, so that it can still be read. It
  doubles what is written and nothing reads it today. The lean is to leave it out.
- **Finding a draft by its words.** A relay that implements NIP-50 takes a `search`
  field, and no other filter finds a draft by a word in its text. The options are to
  require it of a relay that serves drafts or to say nothing. The lean is to say
  nothing: a reader that wants search asks a relay that lists `50`.
- **No number.** A draft has no NIP number and is named by its address. The lean is to
  leave it so: a number is given by whoever accepts the draft into a numbered series,
  and that is outside this document.
