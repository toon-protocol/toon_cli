# Backup and restore

Your agent node is its keys. Lose them and you lose its funds, its identities and its onion
endpoints, which every peer has bound to.

## Back up

```sh
toon wallet backup --out ~/toon-backup.sealed
```

The file holds the mnemonic and the address key of every onion endpoint, sealed under the
wallet passphrase. It refuses to overwrite a file. Keep the file and the passphrase in
different places: either alone is useless, together they are everything.

Back up again after `toon create`, so the new TOON app's onion key is in it.

## Restore from a backup

On a machine with no agent node:

```sh
export TOON_PASSPHRASE_FILE=~/.config/toon/passphrase    # the passphrase of the backup
toon wallet restore ~/toon-backup.sealed
toon init --accept-anyone-terms
```

`restore` recreates the wallet and the address keys; `init` then builds the TOON app on the
keys it finds, at the **same onion endpoints**. A wrong passphrase fails with
`passphrase_wrong`, a file that is not a backup with `keystore_corrupt`, and a home that
already has a wallet with `io`.

## Restore from the mnemonic alone

```sh
printf '%s\n' 'word1 word2 … word12' > /tmp/mnemonic && chmod 600 /tmp/mnemonic
TOON_MNEMONIC_FILE=/tmp/mnemonic toon init --from-mnemonic --accept-anyone-terms
rm /tmp/mnemonic
```

The funds and identities come back. The **onion endpoints do not**: a mnemonic does not hold
their address keys, so the hidden service gets new ones and `init` says so
(`"onion_endpoints_changed": true`). Tell your peers the new addresses.

The mnemonic is never a flag, so it does not land in your shell history.
