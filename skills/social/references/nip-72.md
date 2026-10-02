# NIP-72: moderated communities

A community has moderators who approve posts; clients show the approved ones.

## Kinds

- `34550` community definition, addressable by `d`. Tags: `["d","<name>"]`, `["name","..."]`,
  `["description","..."]`, `["image","<url>"]`, one `["p","<key>","<relay>","moderator"]` per
  moderator, and `["relay","<ws url>","author"|"requests"|"approvals"]`.
- A post in a community: kind `1111` (NIP-22) with `["A","34550:<owner>:<d>","<relay>"]`,
  `["K","34550"]`, `["P","<owner>"]` as its root and the same as its parent for a top-level post.
  An older client uses kind `1` with an `["a","34550:<owner>:<d>"]` tag; read both, write kind `1111`.
- `4550` approval, by a moderator: `["a","34550:<owner>:<d>","<relay>"]`, `["e","<post id>","<relay>"]`,
  `["p","<author>","<relay>"]`, `["k","<post kind>"]`; `content` is the stringified post.
- `10004` lists the communities a key has joined (`nip-51.md`).

## Publish

Define a community (the owner), post, approve (a moderator):

```
toon event publish --kind 34550 --tags '[["d","relay-ops"],["name","Relay ops"],["description","Running relays"],["p","<your key>","","moderator"]]'
toon event publish --kind 1111 --content 'how do I price a write?' --tags '[["A","34550:<owner>:relay-ops","<ws-url>"],["K","34550"],["P","<owner>"],["a","34550:<owner>:relay-ops","<ws-url>"],["k","34550"],["p","<owner>"]]' --relay <ws-url> --yes
toon event publish --kind 4550 --content '<the post, as JSON>' --tags '[["a","34550:<owner>:relay-ops","<ws-url>"],["e","<post id>","<ws-url>"],["p","<author>","<ws-url>"],["k","1111"]]' --relay <ws-url> --yes
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[34550],"#d":["relay-ops"]}'
toon event query <ws-url> --filter '{"kinds":[4550],"#a":["34550:<owner>:relay-ops"]}'
toon event query <ws-url> --filter '{"kinds":[1111],"#A":["34550:<owner>:relay-ops"]}'
```

A post is approved when a moderator listed in the definition signed an approval naming it.

## Notes

- Unapproved posts are still public. Moderation here is display, not enforcement.
- Only approvals signed by a key the definition lists as a moderator (or the owner) count.
