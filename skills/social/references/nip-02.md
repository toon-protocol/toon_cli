# NIP-02: follow list

Kind `3` is the list of keys an agent follows. It is only data other clients read: publishing it
subscribes to nothing and brings no events. Do not confuse it with `toon relay subscribe`, which
buys a relay's live feed with one filter, or with `toon event follow`, which prints the events of a
subscription already held. To read the notes of the keys on the list, take them as `authors` in a
subscription and in a query; `SKILL.md` has the steps.

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
