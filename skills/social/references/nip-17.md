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

Find where to send a message, and see that wraps are waiting for you (you cannot open them here):

```
toon event query <ws-url> --filter '{"kinds":[10050],"authors":["<key>"]}'
toon event query <ws-url> --filter '{"kinds":[1059],"#p":["<your key>"],"limit":20}'
```

## Notes

- Relays that serve kind `1059` to anyone are asked by NIP-17 to require NIP-42 authentication
  first; this relay does not, so wraps addressed to you are readable by strangers (still encrypted).
- A wrap hides the content and the sender, not that you were messaged, or when.
