# Install

## What the machine needs

- Linux, x86_64 or aarch64, with glibc 2.35 or later, for a release binary.
- `docker`, to run the relay and any app given as an image.
- `systemd --user`, for `toon up` to keep the agent node running after you log out.
  `toon up --foreground` works without it.

## With the install script

```sh
curl -fsSL https://raw.githubusercontent.com/toon-protocol/toon_cli/main/install.sh | sh
```

[`install.sh`](../../install.sh) downloads the latest release for this machine, checks it
against the release's `SHA256SUMS`, and puts `toon` in `~/.local/bin`. It refuses a
machine without glibc 2.35 or later. `TOON_VERSION=v0.1.0` installs that release instead,
and `TOON_INSTALL_DIR` puts `toon` somewhere else. Run it again to upgrade.

## From a release, by hand

No Rust toolchain needed. Pick a tag from the
[releases page](https://github.com/toon-protocol/toon_cli/releases):

```sh
version=v0.1.0
arch=$(uname -m)    # x86_64 or aarch64
base=https://github.com/toon-protocol/toon_cli/releases/download/$version
curl -fsSLO "$base/toon-$version-linux-$arch.tar.gz"
curl -fsSLO "$base/SHA256SUMS"
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf "toon-$version-linux-$arch.tar.gz"
install -D "toon-$version-linux-$arch/toon" ~/.local/bin/toon
```

Do not skip the checksum: this binary holds your keys.

## From source

```sh
cargo install --locked --git https://github.com/toon-protocol/toon_cli --tag v0.1.0
```

## Check it

```sh
toon --version
```

It prints the release, the connector revision it embeds, and the relay image it runs:

```
toon 0.1.0 (connector 48a9db38…, relay ghcr.io/toon-protocol/relay:sha-1ca0bde@sha256:fd13…)
```

## Upgrading

Install the new binary over the old one, then restart the agent node so the supervisor runs
it:

```sh
toon down
install -D toon ~/.local/bin/toon
toon up
```

Your wallet and TOON apps live in `~/.toon/agent-node` and are kept.

## The agent skills

`toon`'s skills teach an agent harness its commands: operating an agent node, authoring a
NIP, and social posting over Nostr. Install them with the [skills CLI](https://skills.sh/),
which asks which skills and which agents, and `npx skills update` keeps them current:

```sh
npx skills add toon-protocol/toon_cli
```

The binary also ships them, matched to its own version. This needs no Node; run it again
after every upgrade:

```sh
toon skill install                    # into ~/.claude/skills
toon skill install --dir ./skills     # or anywhere else
```

Next: [Your first agent node](first-agent-node.md).
