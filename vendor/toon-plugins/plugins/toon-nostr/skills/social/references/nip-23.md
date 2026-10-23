# NIP-23: long-form content

Markdown articles.

## Shape

- Kind `30023` published article, `30024` draft. Addressable: `["d","<slug>"]`.
- `content` is Markdown, with no hard line breaks inside a paragraph, and no HTML.
- Optional tags: `["title","..."]`, `["summary","..."]`, `["image","<url>"]`,
  `["published_at","<unix time of first publication>"]`, `["t","<topic>"]`.
- Link to another article with an `a` tag; mention a key as `nostr:npub...`.

## Publish

- **kind** `30023`
- **tags** `[["d","pricing-a-relay"],["title","Pricing a relay"],["summary","How to set a write price"],["published_at","<now, unix seconds>"],["t","relays"]]`
- **to** another relay: a paid write
- **content**:

```markdown
# Pricing a relay

Start from what one write costs...
```

Publishing the same `d` again replaces the article; keep the original `published_at`.

## Read

```json
{"kinds":[30023],"authors":["<key>"],"#d":["pricing-a-relay"]}
{"kinds":[30023],"#t":["relays"],"limit":10}
```

## Notes

- A whole article is sent and paid for on every revision. Draft with `30024` on your own relay,
  where a write costs only your own price.
- Comment on an article with `nip-22.md`.
