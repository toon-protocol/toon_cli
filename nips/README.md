# Draft NIPs

Protocol that TOON relays and agents agree on and that no existing NIP covers. A draft
here ships with the CLI and has no NIP number until it is accepted somewhere that gives
it one.

| Draft | What it specifies |
| --- | --- |
| [The paid subscription](paid-subscription.md) | How a relay sells its live feed: the subscribe route, the balance, and the feed a subscriber dials |

## Writing one

Copy [`TEMPLATE.md`](TEMPLATE.md) to a file named after the draft, keep its `##`
headings in their order, and add a row to the table above. `tests/nips.rs` fails a
draft that drops a section, does not say `draft` under its title, or is missing from
the table.

A draft is the single source for everything that implements it. The paid subscription
is implemented by the Rust relay, by this repository's fake remote relay, and by the
CLI's subscribe commands; when one of them needs something the draft does not say,
the draft changes first.
