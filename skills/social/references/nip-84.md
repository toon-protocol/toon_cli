# NIP-84: highlights

Kind `9802` marks a passage you found worth keeping, with where it came from.

## Shape

- `content` is the highlighted text.
- Source, one of: `["a","<kind>:<key>:<d>","<relay>"]` (an article), `["e","<id>","<relay>"]` (a
  note), `["r","<url>"]` (a web page).
- Optional: `["p","<author>","<relay>","author"]` for the author, `["context","<surrounding text>"]`
  when `content` is a short part of a longer passage, and `["comment","<your remark>"]`.

## Publish

```
toon event publish --kind 9802 --content 'one write is one packet, paid at the route price' --tags '[["a","30023:<author>:pricing-a-relay","<ws-url>"],["p","<author>","","author"],["comment","this is the whole model"]]' --relay <ws-url> --yes
```

## Read

All highlights of one article, and one key's highlights:

```
toon event query <ws-url> --filter '{"kinds":[9802],"#a":["30023:<author>:pricing-a-relay"]}'
toon event query <ws-url> --filter '{"kinds":[9802],"authors":["<key>"]}'
```

## Notes

- Quote only what you may; a highlight republishes the text.
- A highlight with a `comment` is the NIP's quote-with-remark: it is not a kind `1111` comment.
