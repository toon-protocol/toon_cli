# The relay shares the host's network and binds loopback

The relay asks the connector of its own TOON app where a write to it is paid (`TOON_CONNECTOR_URL`), and the connector listens on host loopback. So the relay's container runs with `--network host`, no published port, both its listeners bound to `127.0.0.1` (`TOON_HOST`, `TOON_WRITE_HOST`) on two ports the supervisor picks (`TOON_BLS_PORT` for writes, `TOON_RELAY_PORT` for reads), different for every relay so that two agent nodes on one machine do not collide.

Docker's default bridge was rejected. A container on it cannot reach a listener on the host, on loopback or on the bridge gateway, where the host has a default-deny firewall (`ufw`): the connection times out. Making the connector listen off loopback so that the bridge could reach it would put a listener where ADR 0003 says none should be.

Only the relay is started this way. An app from `toon add --image` or `toon create --image` stays on the bridge, with its port published on loopback.

## Consequences

- A container on the host's network can reach everything on host loopback, the connector's control surface and the overlay's SOCKS proxy among it. That is accepted for the project's own pinned relay image and for no other image.
- Linux only, as the release binaries are. Docker Desktop's host networking is not the same thing and is not supported.
- `docker ps` shows no port for the relay. `toon status --json` reports where it is read on this machine as `read_address`, beside the app's `address`.
- The relay starts before its connector and asks again every few seconds until it answers, so the information document carries the `toon` object within a few seconds of `toon up` reporting. `toon up` does not wait for it.
- The relay re-reads its connector every five minutes. After `toon route price` its document can trail the connector for that long; the relay is not restarted for it.
