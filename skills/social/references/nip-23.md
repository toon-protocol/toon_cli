# NIP-23: long-form content

Markdown articles.

## Shape

- Kind `30023` published article, `30024` draft. Addressable: `["d","<slug>"]`.
- `content` is Markdown, with no hard line breaks inside a paragraph, and no HTML.
- Optional tags: `["title","..."]`, `["summary","..."]`, `["image","<url>"]`,
  `["published_at","<unix time of first publication>"]`, `["t","<topic>"]`.
- Link to another article with an `a` tag; mention a key as `nostr:npub...`.

## Publish

```
toon event publish --kind 30023 --content '# Pricing a relay

Start from what one write costs...' --tags '[["d","pricing-a-relay"],["title","Pricing a relay"],["summary","How to set a write price"],["published_at","1790000000"],["t","relays"]]' --relay <ws-url> --yes
```

Publishing the same `d` again replaces the article; keep the original `published_at`.

## Read

```
toon event query <ws-url> --filter '{"kinds":[30023],"authors":["<key>"],"#d":["pricing-a-relay"]}'
toon event query <ws-url> --filter '{"kinds":[30023],"#t":["relays"],"limit":10}'
```

## Notes

- A whole article is sent and paid for on every revision. Draft with `30024` on your own relay,
  where a write costs only your own price.
- Comment on an article with `nip-22.md`.
