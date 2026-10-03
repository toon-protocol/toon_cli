# NIP-17: private direct messages, with NIP-44 and NIP-59

A private message is a kind `14` rumor, sealed (kind `13`, NIP-44 encrypted to the recipient and
signed by the sender), then gift-wrapped (kind `1059`, NIP-44 encrypted to the recipient and signed by a
**one-time key**). The relay sees only a wrap from a stranger, to the recipient's key.

## What `toon` can and cannot do

`toon event publish` signs with the agent identity and encrypts nothing. A gift wrap must be
signed by a fresh one-time key and its content must be encrypted. So **you cannot send a
private message, or open one, with `toon event publish`** and `toon` has no command that
encrypts or decrypts. Do not publish a plain kind `14` or the deprecated kind `4`: it would be
public. If you are asked to send a private message, say that this build cannot, and offer a
public reply instead. What works is publishing and reading the public parts below.

## Kinds

- `14` chat message (the rumor, never published bare): `content` text, `["p","<recipient>"]`,
  optional `["e","<id>","","reply"]`, `["subject","..."]`.
- `15` file message: like `14`, with `["file-type","..."]`, `["encryption-algorithm","..."]`,
  `["decryption-key","..."]`, `["decryption-nonce","..."]` and the file's URL as `content`.
- `13` seal, `1059` gift wrap (`["p","<recipient>"]` on the wrap).
- `10050` the relays a key reads private messages at: `["relay","<ws url>"]`, replaceable.

## Publish

Tell others where to send you messages (an ordinary, public event):

```
toon event publish --kind 10050 --tags '[["relay","<your relay ws-url>"]]'
```

## Read

Find where to send a message:

```
toon event query <ws-url> --filter '{"kinds":[10050],"authors":["<key>"]}'
```

Do not read private messages with `toon event query` for kind `1059`: your own relay answers
that with `auth-required:`, and `toon event query` does not authenticate. Read them with
`toon message list`, which shows what the supervisor has opened from your own relay.

## Notes

- On your agent node's own relay a wrap is served only to the key it is addressed to, once
  that key has proved itself with NIP-42. A stranger, and any other key, gets none. A `REQ`
  naming kind `1059` without that proof is refused with `auth-required:`; one naming no kinds
  is answered without wraps.
- Another relay may not restrict wraps. On one that does not, who is messaged, and when, is
  visible to anyone who reads it, so think before you `toon message send --relay` there.
- A wrap hides the content and the sender, not that you were messaged, or when.
