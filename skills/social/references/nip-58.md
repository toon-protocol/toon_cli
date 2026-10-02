# NIP-58: badges

A badge is defined by an issuer, awarded to keys, and shown by the keys that wear it.

## Kinds

- `30009` badge definition, addressable. Tags: `["d","<badge id>"]`, `["name","..."]`,
  `["description","..."]`, `["image","<url>","<width>x<height>"]`, optional `["thumb","<url>"]`.
- `8` badge award, by the issuer, immutable. Tags: `["a","30009:<issuer>:<badge id>"]` and one
  `["p","<awardee>"]` per awardee. `content` is empty.
- `30008` profile badges, addressable, `d` is `profile_badges`. Pairs of tags, in order:
  `["a","30009:<issuer>:<badge id>"]` then `["e","<award id>"]`.

## Publish

Define and award (the issuer):

```
toon event publish --kind 30009 --tags '[["d","early-reader"],["name","Early Reader"],["description","Read the relay in its first week"],["image","https://example.org/early.png","256x256"]]'
toon event publish --kind 8 --tags '[["a","30009:<your key>:early-reader"],["p","<awardee key>"]]'
```

Wear a badge you were awarded (the awardee):

```
toon event publish --kind 30008 --tags '[["d","profile_badges"],["a","30009:<issuer>:early-reader"],["e","<award id>"]]'
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[8],"#p":["<key>"]}'
toon event query <ws-url> --filter '{"kinds":[30008],"authors":["<key>"],"#d":["profile_badges"]}'
```

A badge counts only if the award's `a` tag names a definition by the same issuer, and the `p` tag
names the wearer.

## Notes

- Replacing kind `30008` drops the badges you leave out: send the whole list.
- Do not award a badge for payment you cannot show; an award is public and cannot be silently taken back
  except by a deletion request (`nip-09.md`).
