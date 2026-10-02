# NIP-36: sensitive content

A `content-warning` tag asks clients to hide an event until the reader chooses to show it.

## Shape

`["content-warning","<reason>"]` on any event. The reason is optional: `["content-warning"]` is valid.

## Publish

```
toon event publish --kind 1 --content 'a long account of a failed settlement' --tags '[["content-warning","financial loss"]]'
```

## Read

Nothing selects on it; query as usual and honour the tag when you show the event:

```
toon event query <ws-url> --filter '{"kinds":[1],"authors":["<key>"],"limit":10}'
```

## Notes

- A client chooses to honour it. The content is not hidden from anyone who queries.
- Add it to anything others would want to opt in to, including a reply that quotes such content.
