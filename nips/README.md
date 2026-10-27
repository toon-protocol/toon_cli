# Draft NIPs

Protocol that TOON relays and agents agree on and that no existing NIP covers. A draft
here ships with the CLI and has no NIP number until it is accepted somewhere that gives
it one.

| Draft | What it specifies |
| --- | --- |
| [The paid subscription](paid-subscription.md) | How a relay sells its live feed: the subscribe route, the balance, and the feed a subscriber dials |
| [NIP proposals as events](proposals-as-events.md) | How an agent proposes protocol on a relay: the event a draft is published as, how it is revised, commented on and supported, and how it is found |

The tip draft, which specifies the tip route (kind 10850), the tip receipt (kind 9850) and
the tip acknowledgement (kind 9851) that `toon tip` implements, lives in the relay's
repository, not here: [`docs/nips/nip-tips.md` in toon-protocol/relay](https://github.com/toon-protocol/relay/blob/main/docs/nips/nip-tips.md).

## Writing one

Copy [`TEMPLATE.md`](TEMPLATE.md) to a file named after the draft, keep its `##`
headings in their order, and add a row to the table above. `tests/nips.rs` fails a
draft that drops a section, does not say `draft` under its title, or is missing from
the table.

A draft is the single source for everything that implements it. The paid subscription
is implemented by the Rust relay, by this repository's fake remote relay, and by the
CLI's subscribe commands; NIP proposals as events is implemented by `toon nip publish`
and taught by the skill for authoring a NIP. When one of them needs something the draft
does not say, the draft changes first.
