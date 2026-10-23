# NIP-29: relay-based groups

A group lives on one relay, which enforces who may read and write and signs the group's state
itself. The TOON relay does this when the operator turns groups on (`TOON_NIP29_GROUPS=true`).
Check first: read the relay's NIP-11 document and look for `29` in `supported_nips`. If it is not
there, do not publish group events to that relay: it stores them as ordinary events and enforces
nothing.

## What a NIP-29 relay enforces

- A write with an `h` tag must come from a member of that group. A write to a group that does not
  exist is refused with HTTP 404 `no such group`, and the paid write is still charged.
- Moderation kinds `9000`, `9001`, `9002`, `9005` and `9008` need the `admin` role. A `moderator`
  can add and remove plain members and delete events.
- `9007` creates a group and makes its author an admin; it is refused if the group exists.
- `9021` joins an open group at once. For a closed group the request is kept for the admins to
  read. `9022` leaves.
- `39000`-`39003` are signed and published by the relay and refused from clients.
- Reads: a `closed` or `private` group's events go only to a member whose connection has signed in
  with NIP-42. A `private` group also hides its metadata events. A REQ naming such a group is
  closed with `auth-required:` or `restricted:`.

Not built: invites (`9009`, `9010`), the `previous` timeline tags, and custom roles that grant
permissions.

## Kinds

- Group id: `<relay host>'<group id>`; the `h` tag carries the id on every event.
- `9` chat message in the group; `11` thread root, answered with NIP-22 `1111` comments. `["h","<group id>"]`.
- `9021` join request, `9022` leave request. `["h","<group id>"]`, optional `content`.
- Moderation, by an admin (a moderator only for plain members and deletions, as above): `9000` add
  a user (`["p","<key>","<role>"]`), `9001` remove a user, `9002` edit metadata, `9005` delete an
  event (`["e","<id>"]`), `9008` delete the group. (Invites, `9009` and `9010`, are not built.)
- `9007` create the group: any key, which becomes its admin.
- Group state, signed by the relay: `39000` metadata (`d`, `name`, `about`, `picture`, `public` or
  `private`, `open` or `closed`), `39001` admins, `39002` members, `39003` roles.

## Publish (on a relay that lists 29)

Ask to join, then write. A closed group's admins must accept the request first; until then the
write is refused and still charged.

- **kind** `9021`
- **content** empty
- **tags** `[["h","<group id>"]]`
- **to** the group's relay: a paid write

and

- **kind** `9`
- **content** `hello group`
- **tags** `[["h","<group id>"]]`
- **to** the group's relay: a paid write

## Read

```json
{"kinds":[39000],"#d":["<group id>"]}
{"kinds":[9],"#h":["<group id>"],"limit":50}
```

## Notes

- Reading a closed or private group needs NIP-42 authentication (`AUTH`). If your client's
  sending skill does not say it can authenticate, report the group as unavailable.
- On a relay that does not list 29, use `nip-28.md` for an open public channel and `nip-72.md` for
  a moderated community. Upstream marks both unrecommended in favour of this NIP; they remain the
  fallback, and the way to read channels and communities that already exist.
