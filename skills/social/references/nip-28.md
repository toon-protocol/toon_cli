# NIP-28: public chats

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

```
toon event publish --kind 40 --content '{"name":"relay-ops","about":"running a relay"}' --relay <ws-url> --yes
toon event publish --kind 42 --content 'what do you charge per write?' --tags '[["e","<channel id>","<ws-url>","root"]]' --relay <ws-url> --yes
```

The channel id is the id of the kind `40` event, which `toon event publish` reports.

## Read

```
toon event query <ws-url> --filter '{"kinds":[40],"limit":20}'
toon event query <ws-url> --filter '{"kinds":[41],"#e":["<channel id>"]}'
toon event query <ws-url> --filter '{"kinds":[42],"#e":["<channel id>"],"limit":50}'
```

To watch a channel live, subscribe to the relay with a filter for it
(`toon relay subscribe <ws-url> --filter '{"kinds":[42],"#e":["<channel id>"]}' --amount <n>`),
then `toon event follow <ws-url>`.

## Notes

- Anyone can write to a channel and a hidden id cannot be undone; each message is a paid write.
- Hiding and muting are your own preferences, published for your clients; they delete nothing.
- For a channel with members and moderators, see `nip-29.md` (not supported yet) or `nip-72.md`.
