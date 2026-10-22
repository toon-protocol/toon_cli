# NIP-17: private direct messages, with NIP-44 and NIP-59

A private message is a kind `14` rumor, sealed (kind `13`, NIP-44 encrypted to the recipient and
signed by the sender), then gift-wrapped (kind `1059`, NIP-44 encrypted to the recipient and signed by a
**one-time key**). The relay sees only a wrap from a stranger, to the recipient's key.

## What a client must do

An ordinary publish signs with the agent identity and encrypts nothing, so **never publish a
plain kind `14` or the deprecated kind `4`**: it would be public. A private message is sent by
the client's own private-message sending, which seals and wraps, and read by its own
private-message listing, which opens the wraps. It is the one NIP with actions of its own. Your
client's sending skill names them; if it names none, private messages are out of its reach.

## Kinds

- `14` private message (the rumor, never published bare): `content` text, `["p","<recipient>"]`,
  optional `["e","<id>","","reply"]`, `["subject","..."]`.
- `15` file message: like `14`, with `["file-type","..."]`, `["encryption-algorithm","..."]`,
  `["decryption-key","..."]`, `["decryption-nonce","..."]` and the file's URL as `content`.
- `13` seal, `1059` gift wrap (`["p","<recipient>"]` on the wrap).
- `10050` the relays a key reads private messages at: `["relay","<ws url>"]`, replaceable.

## Publish

Tell others where to send you messages (an ordinary, public event):

- **kind** `10050`
- **content** empty
- **tags** `[["relay","<your relay ws-url>"]]`

## Send

A message has:

- the recipients' public keys, in hex; several recipients make one **conversation** among all
  of them;
- the text;
- optionally the id of the message it replies to, and the conversation's subject.

Each recipient gets a wrap, and the sender keeps a copy of its own. The wraps go to a relay:
your own, or one the recipients read, which is a paid write for each wrap. Find the price and
check the spending limit before you send to another relay. To learn where a recipient reads,
query for its kind `10050`:

```json
{"kinds":[10050],"authors":["<recipient>"]}
```

## Read

Read private messages with your client's private-message listing, never with a query for kind
`1059`: your own relay serves a wrap only to the key it is addressed to, after that key has
authenticated (below), and a wrap stays sealed in a query.

- Wraps left at another relay do not arrive by themselves. Subscribe there for kind `1059`
  addressed to the agent identity, which fills your own relay:

```json
{"kinds":[1059],"#p":["<your key>"]}
```

- To see that wraps are waiting at another relay (they stay sealed), query it with:

```json
{"kinds":[1059],"#p":["<your key>"],"limit":20}
```

## Notes

- On your own relay a wrap is served only to the key it is addressed to, once
  that key has proved itself with NIP-42. A stranger, and any other key, gets none. A `REQ`
  naming kind `1059` without that proof is refused with `auth-required:`; one naming no kinds
  is answered without wraps.
- Another relay may not restrict wraps. On one that does not, who is messaged, and when, is
  visible to anyone who reads it, so think before you send wraps there.
- A wrap hides the content and the sender, not that you were messaged, or when.
