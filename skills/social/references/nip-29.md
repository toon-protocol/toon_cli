# NIP-29: relay-based groups

**Not usable yet.** A group lives on one relay, which enforces who may read and write and signs
the group's state itself. The relay of this build does not do that, and it does not implement
NIP-42 authentication, which a group relay uses to know who is asking. The shapes are given so
you can read a group on a relay that does support it. Do not publish group events to a relay
that does not say it supports NIP-29 in its information document: it will store them as
ordinary events and enforce nothing.

## Kinds

- Group id: `<relay host>'<group id>`; the `h` tag carries the id on every event.
- `9` chat message in the group; `11` thread root; `12` reply. `["h","<group id>"]`.
- `9021` join request, `9022` leave request. `["h","<group id>"]`, optional `content`.
- Moderation, by an admin: `9000` add a user (`["p","<key>","<role>"]`), `9001` remove a user,
  `9002` edit metadata, `9005` delete an event (`["e","<id>"]`), `9007` create the group,
  `9008` delete the group, `9009` create an invite (`["code","<code>"]`).
- Group state, signed by the relay: `39000` metadata (`d`, `name`, `about`, `picture`, `public` or
  `private`, `open` or `closed`), `39001` admins, `39002` members, `39003` roles.

## Publish (on a relay that supports it)

```
toon event publish --kind 9021 --tags '[["h","<group id>"]]' --relay <ws-url> --yes
toon event publish --kind 9 --content 'hello group' --tags '[["h","<group id>"]]' --relay <ws-url> --yes
```

## Read

```
toon event query <ws-url> --filter '{"kinds":[39000],"#d":["<group id>"]}'
toon event query <ws-url> --filter '{"kinds":[9],"#h":["<group id>"],"limit":50}'
```

## Notes

- A private group's events are readable only after NIP-42 authentication, which `toon` cannot
  perform. Report the group as unavailable.
- Use `nip-28.md` for an open public channel and `nip-72.md` for a moderated community, which
  work on any relay.
