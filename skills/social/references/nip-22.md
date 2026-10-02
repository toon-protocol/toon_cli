# NIP-22: comments

Kind `1111` comments on anything: an event of any other kind, an addressable event, or an
external thing named by an `I` tag (a URL). Plain text in `content`, no Markdown.

## Shape

Upper-case tags name the **root**; lower-case tags name the **parent** being replied to. A
top-level comment has the same thing in both.

- Root: `["A","<kind>:<key>:<d>","<relay>"]` or `["E","<id>","<relay>","<key>"]` or `["I","<url>"]`,
  with `["K","<root kind>"]` and `["P","<root author>","<relay>"]`.
- Parent: `["a",...]`/`["e",...]`/`["i",...]`, `["k","<parent kind>"]`, `["p","<parent author>"]`.
- A reply to a comment: keep the upper-case tags, and the parent is that comment:
  `["e","<comment id>","","<comment author>"]`, `["k","1111"]`, `["p","<comment author>"]`.

## Publish

A top-level comment on a long-form article (`nip-23.md`):

```
toon event publish --kind 1111 --content 'the second section needs a source' --tags '[["A","30023:<author>:<d>",""],["K","30023"],["P","<author>"],["a","30023:<author>:<d>",""],["e","<revision id>",""],["k","30023"],["p","<author>"]]' --relay <ws-url> --yes
```

## Read

All comments on one root, whichever the depth:

```
toon event query <ws-url> --filter '{"kinds":[1111],"#A":["30023:<author>:<d>"]}'
toon event query <ws-url> --filter '{"kinds":[1111],"#E":["<root id>"]}'
```

## Notes

- Do not use kind `1111` to reply to a kind `1` note: use `nip-10.md`.
- A reply carries the three upper-case tags of its root; leaving them out hides it from the root's query.
- This is the shape the NIP skill uses to comment on a draft NIP (`authoring-a-nip`).
