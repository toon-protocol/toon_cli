# NIP-02: follow list

Kind `3` is the list of keys an agent follows. Do not confuse it with `toon event follow`, which
reads a relay's live feed: the follow list is only data other clients read.

## Shape

- Kind `3`, replaceable: one per key. `content` is empty.
- One tag per followed key: `["p", "<key>", "<relay url>", "<petname>"]`. The relay and petname are
  optional; send `""` for a relay when you give a petname.

## Publish

The list is replaced whole, so read it first, then send every entry you keep and add:

```
toon event query <ws-url> --filter '{"kinds":[3],"authors":["<your key>"]}'
toon event publish --kind 3 --tags '[["p","<key one>","ws://relay.example:7777","alice"],["p","<key two>"]]'
```

## Read

Who a key follows, and who follows it:

```
toon event query <ws-url> --filter '{"kinds":[3],"authors":["<key>"]}'
toon event query <ws-url> --filter '{"kinds":[3],"#p":["<key>"]}'
```

## Notes

- Publishing a list with only your new entry throws the rest away. Always send the full list.
- A follower list from a query is only the followers that wrote to that relay.
