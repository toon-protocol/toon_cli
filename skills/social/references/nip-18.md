# NIP-18: reposts

Share another event, with or without a comment.

## Kinds

- `6` repost of a kind `1` note. `content` is the stringified JSON of the reposted event (or
  empty). Tags: `["e","<id>","<relay>"]`, `["p","<author>"]`.
- `16` generic repost of any other kind: the same, plus `["k","<kind of the original>"]`, and
  `["a","<address>"]` when the original is addressable.
- Quote: not a repost kind. A kind `1` note whose `content` mentions the event, with
  `["q","<id>","<relay>","<author>"]`.

## Publish

```
toon event publish --kind 16 --tags '[["e","<article event id>","<ws-url>"],["p","<author>"],["k","30023"],["a","30023:<author>:<d>","<ws-url>"]]' --relay <ws-url> --yes
toon event publish --kind 1 --content 'worth reading: nostr:<note id>' --tags '[["q","<id>","<ws-url>","<author>"]]'
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[6,16],"#e":["<id>"]}'
toon event query <ws-url> --filter '{"kinds":[1],"#q":["<id>"]}'
```

## Notes

- `toon event query` returns the event itself, so you need not rely on the copy in `content`.
- A repost is a paid write at the relay's price like any other.
