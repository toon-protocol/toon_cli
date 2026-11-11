# On macOS an app is reached through a forwarder

On macOS, docker runs in a Linux VM, whether it is Docker Desktop, OrbStack or a Podman machine, and `toon` supports all three through the `docker` CLI. Two things the agent node relies on on Linux do not hold there. A container on the host's network shares the VM's loopback, not the Mac's, so the relay cannot reach its connector the way ADR 0006 has it (Podman cannot at all; Docker Desktop only with an opt-in setting). And the Mac cannot reach a container's address on a docker network, so the supervisor and the connector cannot reach an app the way ADR 0011 has it (only OrbStack can). A port published on the Mac's `127.0.0.1` works on all three.

So on macOS every app keeps its internal network, as ADR 0011 confines it, and a forwarder stands in front of it: a small container of the project's own, pinned like the relay image, on that internal network and on an ordinary bridge, with one port published on `127.0.0.1`, passing every connection to the app. The connector delivers to that port. The relay is not on the host's network on macOS: it is on a bridge with its two ports published on `127.0.0.1`, and reaches its connector at `host.docker.internal`, which all three runtimes point at the Mac.

We chose this over supporting OrbStack alone, which reaches container addresses and shares the Mac's loopback, so ADR 0006 and 0011 would hold unchanged; but it is one runtime, paid for commercial use, and the operators we expect on a Mac run Docker Desktop or Podman as often. We chose it over publishing the app's own port, which needs the app on an ordinary network and so lets it open connections out.

Linux is unchanged: the relay stays on the host's network and an app is reached at its address, because on Linux a container on a bridge cannot reach host loopback behind a default-deny firewall (ADR 0006).

## Consequences

- Each app on macOS takes one more container and one more network, and the forwarder image is one more pinned image.
- The connector delivers to `127.0.0.1` and a port the supervisor picks, not to the app's address. A forwarder restarted alone is given the port it had.
- An app's `/data` is still bind-mounted from the home and the app still runs as the operator's user and group. If a rootless Podman machine cannot write it, the install guide says to run the machine rootful, and `toon up` fails with `app_failed` naming that fix.
- `toon` drives Podman only through the `docker` CLI and Podman's Docker-compatible socket. It never runs `podman`.
- The relay on macOS reaches everything the Mac serves on loopback through `host.docker.internal`, as it reaches host loopback on Linux. That is accepted for the project's own relay image, as in ADR 0006.
- The rootless docker case of ADR 0011, where the host cannot reach a container's address, is the same problem on Linux. A forwarder there is a separate decision.
