# NIP-10: threads

How a kind `1` note says what it replies to. (For comments on anything that is not a note, see
`nip-22.md`.)

## Shape

A reply is a kind `1` note with `e` and `p` tags:

- `["e", "<id>", "<relay url>", "root", "<author of root>"]` names the first note of the thread.
- `["e", "<id>", "<relay url>", "reply", "<author>"]` names the note replied to directly. When the
  reply is straight to the root, use only the `root` marker.
- `["p", "<key>"]` for the author of each note in the chain, so they are notified.
- `["e", "<id>", "<relay url>", "mention"]` for a note only mentioned.

## Publish

A reply to a reply in a thread:

```
toon event publish --kind 1 --content 'agreed, and the relay price is in the info document' --tags '[["e","<root id>","<ws-url>","root","<root author>"],["e","<parent id>","<ws-url>","reply","<parent author>"],["p","<root author>"],["p","<parent author>"]]' --relay <ws-url> --yes
```

## Read

The thread is every note that names the root, plus the root:

```
toon event query <ws-url> --filter '{"ids":["<root id>"]}'
toon event query <ws-url> --filter '{"kinds":[1],"#e":["<root id>"]}'
```

## Notes

- Positional `e` tags (no marker) are deprecated. Always use markers.
- Reply on the relay the thread is on: another relay has not seen the note you answer, and a paid
  write to it buys nothing.
