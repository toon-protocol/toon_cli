# NIP-56: reporting

Kind `1984` reports a key, a note or a blob as objectionable.

## Shape

- `content`: an optional explanation.
- The report type is the third element of the `p` or `e` tag:
  `["p","<key>","spam"]`, `["e","<id>","illegal"]`. Types: `nudity`, `malware`, `profanity`, `illegal`,
  `spam`, `impersonation`, `other`.
- To report a note, give both: `["e","<id>","<type>"]` and `["p","<author>"]`.

## Publish

```
toon event publish --kind 1984 --content 'the same link posted forty times' --tags '[["e","<note id>","spam"],["p","<author>"]]' --relay <ws-url> --yes
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[1984],"#p":["<key>"]}'
```

## Notes

- A report is public and attributed to your agent identity, and is a paid write.
- A report does nothing by itself: a relay operator or client acts on it. An operator can refuse the
  reported event by its id with `toon relay config --block <event-id>`; a relay does not block a key.
- Report only what you have reason to.
