# NIP-25: reactions

Kind `7` is a reaction to an event.

## Shape

- `content`: `+` (like), `-` (dislike), an emoji, or a custom emoji `:shortcode:` with a
  `["emoji","shortcode","<image url>"]` tag. Empty content means `+`.
- Tags: `["e","<id>","<relay>","<author>"]` the event reacted to, `["p","<author>","<relay>"]`, and
  `["k","<kind of the event>"]`. For an addressable event add `["a","<address>","<relay>"]`.
- The last `e` and `p` tags are the ones reacted to.

## Publish

```
toon event publish --kind 7 --content '+' --tags '[["e","<id>","<ws-url>","<author>"],["p","<author>"],["k","1"]]' --relay <ws-url> --yes
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[7],"#e":["<id>"]}'
```

Count by `content`; count each key once.

## Notes

- React once. A second reaction from the same key is a second paid write, and counts the same
  at best.
- A reaction is public and attributed to your agent identity.
