# Agent keys

An agent key is an operator write key with a **scope** and a **spending limit**, which you
hand to an agent that does not hold the wallet. The agent works the agent node with it and
can do nothing outside its scope ([ADR 0010](../adr/0010-an-agent-key-works-an-agent-node-without-its-home.md)).

The home holds the record of agent keys, `agent-keys.json`. It never holds a private key.
`toon` writes each connector's list of agent keys from the record, to
`connectors/<n>/operator-controller-keys`, and the connector reads that file again on every
signed request. So `create`, `set` and `revoke` reach a running connector at once, without a
restart. (A connector started before this version rendered its config needs one restart to
learn the file's name.)

## Commands

```sh
toon agent-key create [--scope a,b] [--max-per-command N] [--max-per-day N] \
    [--max-restarts N] [--expires 2026-12-31T00:00:00Z] [--toon-app NAME]... [--out FILE]
toon agent-key list
toon agent-key set <public key> [the same options] [--no-expiry]
toon agent-key revoke <public key>
```

- `create` and `set` need the wallet passphrase, as `toon limit set` does. `list` and
  `revoke` do not.
- `list` shows each key's public half, scope, limits, expiry and TOON apps. It never prints
  a private key.
- `revoke` removes the key from the record and from every connector's list. The connector
  refuses the key's next request, a read as well as a write.
- `set` changes only what you name. `--scope` and `--toon-app` replace the whole list.

## Scope

| Scope | What it lets the key do |
|---|---|
| `status`, `logs`, `apps`, `route-price`, `sign`, `read-messages` | The orders the supervisor answers for the key. They move no money. |
| `send` | Originate packets (`POST /packets`) |
| `peer` | Add and remove peerings |
| `routes` | Write forwarding routes, runtime and leased |
| `channels-open`, `channels-fund`, `channels-withdraw`, `channels-land` | The channel writes, as the connector names them |

With no `--scope` the key gets every order scope and `send`, `peer` and `routes`: nothing
that withdraws from or closes a channel. The channel scopes must be named. A key whose scope
holds only orders is on no connector's list, because a connector's list takes only writes.

`sign` lets the agent sign anything as the agent identity; the secret stays in the home.

## Limits and expiry

`--max-per-command` and `--max-per-day` are in the token's base units, and the wallet's
limits if omitted. The connector holds them: it counts what the key sends in packets and
what peering and channel writes deposit, per write and per UTC day, so the limit holds
although the agent never opens the home. A key granted on several TOON apps has the limit on
each, so it can spend that much per day on each. `--max-restarts` is how many times a day
the key may restart an app (ten if omitted). `--expires` is an RFC 3339 time after which the
connector refuses the key.

## Which TOON apps

`--toon-app` names a TOON app the key is granted on, and may be repeated. With none, the key
is granted on the relay TOON app only. The key is on the list of the TOON apps it is granted
on and of no other.

## The key file

`create` writes one file, readable by its owner only, to `--out` or to
`agent-key-<first 8 hex of the public key>.json` in the current directory. It refuses to
overwrite a file. The file is JSON:

```json
{
  "private_key": "<64 hex: the ed25519 secret key>",
  "public_key": "<64 hex>",
  "supervisor": "http://127.0.0.1:12345",
  "toon_apps": { "relay": "http://127.0.0.1:4000" }
}
```

- `private_key` is the only copy. Hand the file to the agent and keep none where it can read
  it; if it leaks, `toon agent-key revoke` the key.
- `supervisor` is where the supervisor listens for orders, on loopback.
- `toon_apps` holds the operator-surface address of each TOON app the key is granted on.
- The file is written once. `toon agent-key set --toon-app` changes which connectors accept
  the key but not a file already handed out: give the agent the address of a newly granted
  TOON app, or revoke the key and create a new one.

## Orders

The supervisor answers orders: HTTP requests signed with an agent key (RFC 9421, the
signature a connector wants of a write: `@method`, `@path` and `content-digest`, with
`created`, `expires` and `keyid` the key's public half). It listens on the `supervisor`
address of the key file, on loopback only, beside its local socket. An address in the record
that is not loopback is never bound.

| Order | Scope | Body | Answer |
|---|---|---|---|
| `POST /orders/status` | `status` | none | The TOON apps the key is granted on, with their connector and apps. No file paths. |
| `POST /orders/logs` | `logs` | `{"toon_app": NAME, "lines": N}`, both optional | The last lines of that TOON app's connector log. |

The supervisor reads the record on every order, so a revoke or a change takes effect on the
next one. It refuses, with `401`, an unsigned order, a key not on the record, a signature that
does not verify or has expired, and a signature it has already answered. It refuses with
`403` a key past its `--expires`, and a key whose scope lacks the order's; that answer names
the scope (`error.scope`). A refused order does not spend its signature. An order it cannot
carry out, such as logs of a TOON app the key is not granted on, is answered `400`. Every
refusal is `{"error": {"code", "message"}}`.

## Running `toon` with an agent key

`TOON_AGENT_KEY=<key file>` or `--agent-key <key file>` makes `toon` run as the agent: it reads
the key file and nothing else. It never looks for a home or a keystore, even if `$HOME` holds
one, and it holds no bearer token. A command goes out either as an order to the supervisor
(`status`, `logs`) or as a signed write to the operator surface of a TOON app the key file
names (`send`, `peer add`). The reads those need are signed with the key too.

```sh
export TOON_AGENT_KEY=/run/secrets/agent-key.json
toon status
toon logs relay --connector -n 20
toon send g.toon.<segment>.relay --amount 10 --yes
toon peer add https://other.example/ilp --deposit 1000 --yes
toon limit show
```

| Command | What it does with a key |
|---|---|
| `toon status` | The `status` order: only the TOON apps the key is granted on |
| `toon logs <toon app>` | The `logs` order: that TOON app's connector log (an app's own log is not offered) |
| `toon send` | A packet originated by a write signed with the key (`send` scope); sealed to the TOON app's connector unless `--seal-to` names another, dialled directly |
| `toon peer add` | A peering written with the key (`peer` scope) |
| `toon limit show` | The key's spending limit and what is left of today's, read from each connector the key file names (`--app` picks one) |

`--app <toon app>` picks which TOON app's connector `send` and `peer add` use. Without it, the
key file's only TOON app is used, else `relay`.

- `--yes` is still required to pay; without it nothing is sent (`not_confirmed`).
- `toon` keeps no count of spending in this mode. The connector counts and refuses: a write over
  the key's spending limit (or deposit limit) exits with `spending_limit`; one outside the
  key's scope says `scope` in the connector's refusal, with the code of the command
  (`send_failed`, `peer_failed`).
- After `toon agent-key revoke` the same key file is refused by the supervisor and by every
  connector, for reads and writes.
- Every other command answers `needs_home` (exit 1) before anything else is tried, and the
  message names the command and says the operator must run it: `wallet`, `limit set`, `init`,
  `create`, `up`, `down`, `agent-key`, and also, until their tickets land, `add`, `remove`,
  `route`, `event` and `message`.
- An agent whose loopback cannot reach the supervisor's order address needs a tunnel the
  deployer provides; reaching an agent node on another host is not offered.
