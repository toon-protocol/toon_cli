# Creating a TOON app

`toon create` starts a new **TOON app**: a second connector, with its own identity, its own
settlement keys, its own address segment and, by default, its own onion endpoint, and an
app behind it. Both are named after the TOON app.

## Add, or create?

| You want | Use |
| --- | --- |
| Another service sold under the connector you already run | [`toon add`](adding-an-app.md) |
| A service with its own identity, address segment and onion endpoint | `toon create` |
| Its own peerings, channels, or forwarding fees | `toon create` |
| One connector's packets not to affect another's (restarts, load) | `toon create` |

A second connector costs a second set of keys to fund and, unless you say otherwise, two
channels' worth of deposits.

## Walkthrough: create a TOON app

**1. Pick an app.** Any image that follows the
[app contract](adding-an-app.md#what-an-app-is), or `--url` for one you already serve.

**2. Create it.** By default the new connector peers with the first TOON app in both
directions, a channel each way opened with `--deposit`, plus a route each way. That moves
money twice:

```sh
toon create search --image search-app --price 10 \
  --deposit 1000000 --accept-anyone-terms --yes --json > create.json
```

The first attempt usually fails with `unfunded`: the new connector's settlement key is
empty. Its message names the address. Fund it as you funded the first
([Money](money.md#funding)), then run the same command again. Nothing was created by the
failed attempt.

**3. Look at what you made.**

```sh
jq .created create.json         # what was created: its ILP address, where it listens, ...
toon status                     # two TOON apps
toon --app search peer list     # its peering back toward the first
toon limit show                 # down by 2 × the deposit
```

**4. Pay the new app from the first connector.** It is behind another connector now, so
seal to that one:

```sh
SEARCH=$(jq -r .created.listen create.json)
toon send "$(jq -r .created.address create.json).search" --amount 10 \
  --seal-to "http://$SEARCH/ilp" --yes
```

`--seal-to` is only where `toon` reads the key to seal to; on this machine the new
connector's loopback address answers that.

## Options

- `--app <from>`: the TOON app to peer with, instead of the first one.
- `--no-peer`: create no peerings and deposit nothing. The new connector then reaches no
  one until you [peer it](peering.md) yourself, with `--app <name>` on `peer add`.
- `--clearnet <hostname>` and `--listen`: reach it at a public hostname instead of as a
  hidden service, as for `init` ([Networks and reachability](networks-and-reachability.md)).

## Work on a second TOON app

Every command that talks to a connector takes `--app <name>`, and without it talks to the
first TOON app:

```sh
toon --app search route list
toon --app search channel list
toon add notes --to search --image notes-app --yes   # add puts the app where --to says
```

## Destroy a TOON app

```sh
toon destroy search
```

`destroy` stops the connector and removes its files and its app's data, but not its onion
endpoint's key. It needs the agent node
running, to read the channels, and refuses with `funds_held`, naming each channel, while
any channel still holds collateral or an unlanded voucher. Withdraw and land them first
([Money](money.md#channels)):

```sh
toon --app search channel list
toon --app search channel withdraw <id>     # outbound
toon --app search channel land <id>         # inbound
```

An agent node always keeps one TOON app: `destroy` refuses the last one with
`last_toon_app`.
