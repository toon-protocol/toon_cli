# NIP-22: comments

Kind `1111` comments on anything: an event of any other kind, an addressable event, or an
external thing named by an `I` tag (a URL). Plain text in `content`, no Markdown.

## Shape

Upper-case tags name the **root**; lower-case tags name the **parent** being replied to. A
top-level comment has the same thing in both.

- Root: `["A","<kind>:<key>:<d>","<relay>"]` or `["E","<id>","<relay>","<key>"]` or `["I","<url>"]`,
  with `["K","<root kind>"]` and `["P","<root author>"]`.
- The relay in an `A`, `E`, `a` or `e` tag is the `<ws-url>` the event is on, or `""` when you do
  not know it. A key may be followed by a relay hint in `P` and `p`, but none is needed.
- Parent: `["a",...]`/`["e",...]`/`["i",...]`, `["k","<parent kind>"]`, `["p","<parent author>"]`.
- A reply to a comment: keep the upper-case tags, and the parent is that comment:
  `["e","<comment id>","","<comment author>"]`, `["k","1111"]`, `["p","<comment author>"]`.

## Publish

A top-level comment on a long-form article (`nip-23.md`):

- **kind** `1111`
- **content** `the second section needs a source`
- **tags** `[["A","30023:<author>:<d>","<ws-url>"],["K","30023"],["P","<author>"],["a","30023:<author>:<d>","<ws-url>"],["e","<revision id>","<ws-url>"],["k","30023"],["p","<author>"]]`
- **to** the article's relay: a paid write

## Read

All comments on one root, whichever the depth:

```json
{"kinds":[1111],"#A":["30023:<author>:<d>"]}
{"kinds":[1111],"#E":["<root id>"]}
```

## Notes

- Do not use kind `1111` to reply to a kind `1` note: use `nip-10.md`.
- A reply carries the three upper-case tags of its root; leaving them out hides it from the root's query.
- This is the shape the NIP skill uses to comment on a draft NIP (`authoring-a-nip`).
