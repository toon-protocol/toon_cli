# A lease's secret is kept in plaintext with whatever runs the command

Renting TOON Compute mints a root secret for each lease, and every request that reads, stops or rotates the lease presents a token derived from it (TOON Compute spec §6.1.1). The provider stores only that token and nothing about the tenant, so whoever loses the secret loses the lease: nobody can read its access, terminate it or rotate it again, and it runs, paid, until it expires. So `toon` keeps one file per lease, holding its root secret, its workload id, provider, Listing and version, its `expires_at`, what it has paid and its lease limit, and writes it before the spawn packet leaves. A spawn whose file cannot be written is not sent.

The file is kept with whatever runs the command: in the home's `leases/` when the operator runs `toon`, and in a `leases/` directory beside the agent key file when an agent runs it with an agent key and no home (ADR 0010). It is plaintext, readable only by its owner, never printed, and `toon backup` carries the home's.

We chose plaintext over encrypting it with the keystore's passphrase, because an agent that holds no passphrase must still be able to read, stop and rotate the leases it took, as the supervisor keeps the agent identity's secret for the same reason (ADR 0008). We chose a store per agent key over keeping every lease in the home, because an agent key's holder never sees the home, and over asking the supervisor to hold the secrets, which would put the supervisor in the path of a payment ADR 0010 keeps it out of.

## Consequences

- Whoever can read the file controls the lease. It is a secret like the agent key file beside it, and an agent's operating-system user should keep it as one.
- A lease taken with an agent key is not in `toon backup`. Its holder backs up the directory beside the key, or loses those leases with it.
- A lease file is never deleted by `toon` on its own; `toon compute forget` removes one, and warns when the lease may still be live.
- A rotation writes the new root secret into the file before any member is asked to rotate, and keeps the old one until every member has confirmed.
