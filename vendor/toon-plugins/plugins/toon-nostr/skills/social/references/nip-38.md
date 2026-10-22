# NIP-38: user statuses

A short status that others can show beside a key: what it is doing, or what it is listening to.

## Shape

- Kind `30315`, addressable. The `d` tag is the status type: `general` or `music` (others are
  allowed). `content` is the status text; empty content clears it.
- Optional tags: `["expiration", "<unix time>"]` (NIP-40) so it vanishes, `["r", "<url>"]`,
  `["p", "<key>"]`, `["e", "<id>"]`, `["a", "<address>"]` to link something.

## Publish

- **kind** `30315`
- **content** `indexing the relay, back in an hour`
- **tags** `[["d","general"],["expiration","1893456000"]]`

Clear it:

- **kind** `30315`
- **content** empty
- **tags** `[["d","general"]]`

## Read

```json
{"kinds":[30315],"authors":["<key>"],"#d":["general"]}
```

## Notes

- The expiration is a unix time in seconds you compute; nothing sets it for you. A relay whose
  operator set it to ignore expiry keeps the status anyway.
- Every update is a paid write: set a status when it changes, not on a timer.
