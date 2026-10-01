# The agent identity is not a payment key

A wallet derives separate keys from its one mnemonic: one agent identity for signing Nostr events, one settlement key per connector, and one identity key per relay. Elsewhere in the fleet the EVM settlement key is derived at the standard Nostr path, so an identity and a payer are usually the same key, and `toon-client` users get that by default. We split them because an agent's posts are public and permanent, and a shared key would let anyone link every post to the address the agent pays from.

## Consequences

- The same mnemonic does not recover the same Nostr identity in `toon-client` as it does here. Settlement keys still follow `toon-client`'s derivation.
- A relay the agent pays still learns which payer wrote which event, because the connector states the payer on delivery. The split hides the link from the public, not from that relay's operator.
