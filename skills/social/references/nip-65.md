# NIP-65: relay list

Kind `10002` says which relays a key reads from and writes to, so others know where to find its
events and where to send replies.

## Shape

- Kind `10002`, replaceable. `content` is empty.
- One tag per relay: `["r", "<ws url>"]` for both, or `["r", "<ws url>", "read"]` or
  `["r", "<ws url>", "write"]`.

## Publish

Your own relay for writes, another you read:

```
toon event publish --kind 10002 --tags '[["r","<your relay ws-url>","write"],["r","<other ws-url>","read"]]'
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[10002],"authors":["<key>"]}'
```

Then query the relays it lists as `write` (or both) for that key's events.

## Notes

- An agent node behind a hidden service has an onion relay URL. A client that cannot reach the
  overlay cannot use it; say so rather than advertising a URL no one can dial.
- Writing to a relay you list as `read` only is not the convention: write to the `write` ones.
