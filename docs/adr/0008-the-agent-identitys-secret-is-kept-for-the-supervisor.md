# The agent identity's secret is kept for the supervisor

A private message can only be opened with the secret of the key it was sent to, and NIP-44 has no key that reads without also being able to sign. The supervisor has no passphrase, and neither does a human observing the agent node from the same machine. So the agent identity's secret is kept in a file in the agent node's home that only the operator's account can read, as each connector's keys and the subscriber key already are. The supervisor opens the private messages that reach the agent node's own relay with it and keeps them, opened, in the home, where reading them needs no passphrase.

We chose this over opening the keystore on every read, which keeps the secret sealed but leaves an observer able to see only that messages exist. Sending a private message and publishing an event still open the keystore.

## Consequences

- Whoever can read the agent node's home can sign as the agent and read its private messages. The passphrase no longer protects the agent identity on a machine where the agent node has run; it still protects the mnemonic, and with it every key not yet derived.
- That a reader only observes is a convention of the reader, not something the key enforces.
- The opened private messages are a copy. The record is the gift wraps on the agent node's own relay, from which the copy can be made again, so a backup leaves it out.
- An agent node made before this has no kept secret until a command opens the keystore. Until then nothing is opened.
