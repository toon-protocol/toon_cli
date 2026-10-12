# An app is confined and reaches nothing unless allowed

An operator may add any image, and an agent may propose one (ADR 0010). An app is payment-oblivious, so it needs nothing but the port its connector delivers to. So `toon` starts every app's container confined: on a network of its own that leads nowhere, as the operator's own user and group, with no capabilities and no way to gain any, a read-only root filesystem, and limits on memory, CPU and processes. `/data` and a small `/tmp` are the only places it can write.

A network that leads nowhere is one of docker's internal networks, and docker publishes no port on those. So the supervisor reaches an app at its container's address on that network, not at a port published on loopback. This replaces the last paragraph of ADR 0006 for every app but the relay.

An operator can make an allowance for one app when adding it: that it may open connections out, a further scratch path that is emptied on every restart, or other limits. An app allowed to open connections out gets an ordinary network of its own in place of the internal one. There is no allowance for a capability, for another user, or for running unconfined.

We chose this over a network per app with connections out left open, which keeps apps apart from each other but lets an image nobody has read reach the internet and whatever the host leaves listening. We chose the operator's user over the image's own because a root that has lost its capabilities cannot write a `/data` the operator owns, and a `/data` made writable by anyone, or a kept capability, is a weaker confinement to explain.

## Consequences

- The relay is confined the same way except for its network, which stays the host's (ADR 0006). It can still reach everything on host loopback.
- An image that needs root, a capability, or a path of its own to keep data in is not an app `toon` runs. It fails with `app_failed`.
- An app that calls a chain or an outside API must be added with the allowance to open connections out. Without it the app starts and its calls fail.
- Rootless docker is not supported: there the host cannot reach a container's address.
- Each app takes one docker network, and docker's default address pools hold about thirty.
- Confinement applies to every app from the next `toon up`, with no record of which apps predate it. An app that ran before and no longer starts needs an allowance or a new image.
- An allowance is set when the app is added. To change it, remove the app and add it again; the relay, which cannot be removed, keeps the defaults.
- Docker gives a capability only to root, so with the operator's user and no new privileges a capability allowance could not work even if one were wanted.
