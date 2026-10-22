# NIP-51: lists

Lists of things a key keeps: mutes, pins, bookmarks, relays, interests, follow sets.

## Kinds

| Kind | List | Items |
| --- | --- | --- |
| `10000` | Mute list | `p`, `t`, `word`, `e` |
| `10001` | Pinned notes | `e` |
| `10003` | Bookmarks | `e`, `a` |
| `10004` | Communities | `a` (kind `34550`) |
| `10005` | Public chats | `e` (kind `40`) |
| `10006` | Blocked relays | `relay` |
| `10007` | Search relays | `relay` |
| `10009` | Simple groups | `group`, `r` |
| `10015` | Interests | `t`, `a` |
| `30000` | Follow set | `p` |
| `30002` | Relay set | `relay` |
| `30003` | Bookmark set | `e`, `a` |
| `30004` | Curation set | `e`, `a` |

`100xx` lists are replaceable (one per key). `300xx` are sets: addressable, with a `d` tag and
optional `title`, `description` and `image` tags.

## Publish

A mute list, public entries only:

- **kind** `10000`
- **content** empty
- **tags** `[["p","<key to mute>"],["word","spoiler"]]`

A named follow set:

- **kind** `30000`
- **content** empty
- **tags** `[["d","friends"],["title","Friends"],["p","<key>"],["p","<other key>"]]`

## Read

```json
{"kinds":[10000],"authors":["<key>"]}
{"kinds":[30000],"authors":["<key>"],"#d":["friends"]}
```

## Notes

- A list is replaced whole: query it, copy, add, publish all.
- The standard also lets a list hold private items, NIP-44 encrypted in `content`. An ordinary
  publish encrypts nothing (only private messages are sealed, see `nip-17.md`), so unless your
  client's sending skill says it can seal and open list items, keep to public tags and treat a
  non-empty `content` you read as private items you cannot open.
- Do not put something in a public list you would not publish.
