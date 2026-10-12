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
each, so it can spend that much per day on each. `--max-restarts` is how many times an hour
the key may restart a connector, which setting a route's price does (six if omitted; change
it with `toon agent-key set`). Past it the order is refused with `rate_limited`. `--expires` is an RFC 3339 time after which the
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
| `POST /orders/add` | `apps` | `{"name", "to", "image", "address", "price", "request", "yes"}`, and the flags `toon add` takes that an agent key may not use | What `toon add` reports, as `{"result", "text"}`. See the list of trusted images below. |
| `POST /orders/remove` | `apps` | `{"app", "yes"}` | What `toon remove` reports, as `{"result", "text"}`. |
| `POST /orders/sign` | `sign` | `{"kind": N, "content": "", "tags": [], "toon_app": NAME}`, `toon_app` optional | `{"event": <signed event>, "relay": <the relay's write address>}`, the relay when `toon_app` is named. Any kind: signing is not bounded by it. |
| `POST /orders/seal` | `sign` | `{"recipients": [...], "content": "", "reply_to", "subject", "toon_app"}` | The rumor and the sealed, signed wraps of a private message (`to_recipients`, `to_sender`), and the relay as above. Nothing is sent. |
| `POST /orders/messages` | `read-messages` | `{"with": [...], "since": N, "limit": N}`, all optional | What `toon message list` prints with the home, and its `text`. |

No answer holds the agent identity's secret. The supervisor signs with the copy kept in the
home (ADR 0008); `toon agent-key create` and `set` keep it when the scope holds `sign` or
`read-messages`. An order that finds none is answered `agent_key_not_kept`.

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
| `toon logs <toon app> --connector` | The `logs` order: that TOON app's connector log. An app's own log is not offered, so without `--connector` it answers `usage` |
| `toon send` | A packet originated by a write signed with the key (`send` scope); sealed to the TOON app's connector unless `--seal-to` names another, dialled directly |
| `toon peer add` | A peering written with the key (`peer` scope) |
| `toon add --image <image> --to <toon app>` | The `add` order (`apps` scope): the app is added at once if its image is on the list of trusted images, as `toon add` does with the home. Any other image, and every `--url`, becomes a pending app and answers `approval_pending` |
| `toon remove <app>` | The `remove` order (`apps` scope): any app behind a TOON app the key is granted on, but the relay |
| `toon route price <prefix> <price> --yes` | The `route-price` order: sets the price of the route to an app on a TOON app the key is granted on, and restarts that connector. Under the key's restart limit: past it, `rate_limited`, and the message says when the next restart is allowed |
| `toon event publish` | The `sign` order, then the packet is sent with the key (`send` scope) to the relay of the TOON app, so the connector counts `--amount` against the key. `--relay` needs the home |
| `toon event sign` | The `sign` order: prints the signed event and sends nothing |
| `toon message send` | The `seal` order, then each wrap is sent with the key as above. `--relay` needs the home |
| `toon message list` | The `messages` order (`read-messages` scope) |
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
  `create`, `up`, `down`, `agent-key`, and also, until their tickets land, `route` (except
  `route price`), `event query`, `event watch`, `nip` and the subscribe commands, and `event publish` and `message send` with `--relay`.
- An agent whose loopback cannot reach the supervisor's order address needs a tunnel the
  deployer provides; reaching an agent node on another host is not offered.

## The list of trusted images

The home holds `trusted-images`, a text file `toon init` writes seeded with one line,
`ghcr.io/toon-protocol/*`. It decides what an agent key may add at once. The operator edits it
with any editor; the supervisor reads it on every add, so a change needs no restart. A home
that has no such file trusts the seed, and an empty file trusts nothing.

```text
# Blank lines and lines that start with # are ignored.
ghcr.io/toon-protocol/*          # every repository below that prefix
example.com:5000/team/app        # exactly this repository
```

- An entry is a repository, or a prefix ending in `/*`. A line that is neither is ignored.
- An image matches by its repository, so `ghcr.io/toon-protocol/echo`, `…/echo:1.2` and
  `…/echo@sha256:…` are one entry's. A `..` or empty path part never matches.
- The list is checked for an agent key only. An operator running `toon add` with the home is
  never checked against it.

An add by an agent key, with `apps` in its scope and granted on the TOON app it goes behind
(`--to`), is added at once only if its image is on the list **and** it carries no allowance
(`--allow-egress`, `--scratch`, `--memory`, `--cpus`, `--pids`) and no variable (`--env`): a
trusted image is trusted as shipped. Anything else, and every `--url`, becomes a **pending
app**: it is recorded in the home, nothing is pulled, started or routed, and the add answers
`approval_pending`, exit 1, naming the app. The same add again gives the same answer and
records nothing more. A key holds at most 8 pending apps; the ninth is refused with
`needs_operator` and not recorded. A secret (`--secret`, `--secret-file`) is refused with
`needs_home`. `toon remove` with a key refuses the relay with `needs_operator`.

### Pending apps

`toon status` shows a pending app to the operator, with the image or URL, the TOON app, the
allowances and variables asked for, and which key asked. An agent key's `toon status` shows
only the ones that key asked for, as `pending`, or as `refused` with the operator's reason.

- `toon approve <app>` (home only) adds the app exactly as it was asked for: the same image
  reference or URL, allowances and variables, behind the same TOON app. It restarts that
  connector, as `toon add --yes` does. Approving does not put the image on the list of trusted
  images; that is an edit to the `trusted-images` file.
- `toon refuse <app> [--reason <text>]` (home only) records the refusal. The app is not added.
  The key may ask again, which replaces the refusal with a new pending app. The home keeps
  the latest 8 refusals of each key and forgets older ones.
- A name held by a pending app is not taken by another key's add, nor by the same key's add
  of something else: both answer `name_taken`.
- With an agent key both answer `needs_home`.

Pending apps do not expire. Revoking a key drops its pending apps, and so does adding an app
of the same name with the home. The supervisor logs each pending app, approval and refusal.
