# NIP-28: public chats

Upstream marks this NIP unrecommended: try NIP-29 (`nip-29.md`) on a relay whose `supported_nips`
lists `29`. Use this one as the fallback on a relay that does not, and to read channels that exist.

Open channels anyone can write to. This NIP's kind `42` is a channel message; it has nothing to do
with NIP-42 (authentication), which is kind `22242`.

## Kinds

- `40` create a channel. `content` is JSON: `{"name":"...","about":"...","picture":"..."}`.
- `41` set channel metadata. Same JSON in `content`; tag `["e","<channel id>","<relay>"]`. Only the
  creator's `41` counts.
- `42` channel message. `content` is the text. Tags: `["e","<channel id>","<relay>","root"]`; a reply
  adds `["e","<message id>","<relay>","reply"]` and `["p","<author>","<relay>"]`.
- `43` hide a message (for you): `["e","<message id>"]`, `content` an optional JSON `{"reason":"..."}`.
- `44` mute a user (for you): `["p","<key>"]`, `content` an optional JSON reason.

## Publish

Create a channel:

- **kind** `40`
- **content** `{"name":"relay-ops","about":"running a relay"}`
- **tags** none
- **to** another relay: a paid write

Write to it:

- **kind** `42`
- **content** `what do you charge per write?`
- **tags** `[["e","<channel id>","<ws-url>","root"]]`
- **to** the channel's relay: a paid write

The channel id is the id of the kind `40` event, which publishing it reports.

## Read

```json
{"kinds":[40],"limit":20}
{"kinds":[41],"#e":["<channel id>"]}
{"kinds":[42],"#e":["<channel id>"],"limit":50}
```

To watch a channel live, subscribe to its relay with `{"kinds":[42],"#e":["<channel id>"]}`,
then watch your own relay with the same filter.

## Notes

- Anyone can write to a channel and a hidden id cannot be undone; each message is a paid write.
- Hiding and muting are your own preferences, published for your clients; they delete nothing.
- For a channel with members and moderators, see `nip-29.md` where the relay lists 29, or `nip-72.md`.
