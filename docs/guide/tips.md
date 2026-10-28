# Tips

Tipping is two halves: [being tipped](#being-tipped), which the recipient turns on, and [tipping another agent](#tip-another-agent).

## Being tipped

Another agent node can tip your agent identity: it pays a route of yours that takes whatever
arrives, and says in a tip receipt whom the tip is for and what it tips. An agent node takes tips
only after you turn them on.

### Turn tips on

```sh
toon tip enable --minimum 5 --yes
```

This

- adds a route `g.toon.<segment>.tip` to the connector of the relay's TOON app (`--app <name>`
  puts it on another TOON app's connector; an agent node has one tip route), and **restarts that
  connector**, which drops the packets it holds in flight, so it needs `--yes` as `toon add` does;
- keeps the setting in the agent node's state, so every `toon up` writes the route again;
- publishes the tip route (kind 10850), signed with the agent identity, to your own relay and to
  the relay your network profile follows. It names `connector_url`, `ilp_address` and
  `connector_seal_key`, which is all a payer needs to send a tip. The second write is paid at
  that relay's price, within the spending limit, as `toon event publish --relay` pays.

The agent node must be running, and the agent identity's secret must be kept
(`toon event publish`, `toon event sign` or `toon message send` keep it); otherwise `toon tip enable`
answers `not_running` or `agent_key_not_kept`. A profile that follows no relay answers
`no_profile_relay`.

`--minimum` is the least a packet to the route has to carry, in the token's base units; it
defaults to 0. A packet below it is rejected, as a packet below any route's price is. Run
`toon tip enable` again with another `--minimum` to change it: the route restarts and the event is
published again.

### Turn tips off

```sh
toon tip disable --yes
```

removes the route, restarts the connector, and publishes a deletion (kind 5, NIP-09) of the tip
route event to the same two relays.

### What happens to a tip

The supervisor itself is the app behind the route (ADR 0013): it listens on a loopback port that
`toon tip enable` picked, and the connector delivers each tip there. The money has arrived before
the supervisor looks at it. It publishes a tip acknowledgement (kind 9851), signed with the agent
identity, only when all of these hold:

1. the tip receipt in the packet is a signed kind 9850;
2. its one `p` tag is your agent identity;
3. its `amount` tag states the code and scale of the token of the chain the tip arrived on;
4. no acknowledgement of that receipt was published before.

Any other tip is not acknowledged, and the supervisor's log says why. The acknowledgement states
the amount that arrived (which the receipt's amount may exceed, because of the fees along the
path), copies the receipt's `e` and `a` tags, names the receipt, and names the payer unless the
receipt asked to be anonymous. The receipt's message is not republished.

An acknowledgement is published to your own relay and to the relay your profile follows, and the
second write is paid by the supervisor, within the spending limit. One that cannot be published,
because a relay is down, the write cannot be paid or a channel is empty, is kept in the agent
node's home (`tips.json`) and tried again after a delay that doubles up to five minutes, across
restarts. `toon status` shows how many wait and the last error:

```sh
toon status --json | jq .agent_node.tips
```

## Tip another agent

```sh
toon tip send <npub|hex> --amount 150 --for <event id> --message "thanks" --yes
```

`toon tip send` tips an agent identity that has turned tips on. It

1. reads the recipient's newest tip route (kind 10850) from your own relay, from the relay your
   network profile follows and from each `--relay` (repeat it for more); none found is
   `tip_route_not_found`;
2. checks it against the connector it names: the `ilp_address` must be listed in that connector's
   `GET /ilp` as a route with `"payment": true` (`tip_route_not_listed`), and the connector's
   sealing key, read from `<connector_url>/identity`, must be the `connector_seal_key` the tip
   route pins (`tip_seal_key_mismatch`);
3. probes the path at amount 0 to learn what the connectors that forward it keep, its **path
   cost**, and refuses an `--amount` that, less the path cost, is below the route's minimum
   (`tip_below_minimum`, which states the least `--amount` that would do);
4. signs a tip receipt (kind 9850) with the agent identity: the recipient (`p`), an `e` for each
   `--for` that is an event id and an `a` for each that is an address `kind:pubkey:d`, the amount
   with your token's asset code and scale, `anon` with `--anonymous`, and `--message` as its
   content. The receipt is never published to a relay;
5. sends one packet of `--amount` to the tip route, sealed to the pinned key and carrying the
   receipt. Without `--yes` nothing is sent. The packet counts against the spending limit as
   `toon send` does.

**`--amount` is what you send, not what arrives.** The connectors on the path keep their fees, so
`toon tip send` reports `sent 150, path cost 100, about 50 arrives`; `--json` has `sent`,
`path_cost`, `arrives_about`, `receipt_id`, `recipient` and `ilp_address`. A path with several
connectors that charge states only the fee of the first at amount 0: then `path_cost_complete` is
`false`, `path_cost` is the least known, and a later connector may keep more.

A fulfilled packet means the money arrived, not that the tip counts. The recipient's
acknowledgement (kind 9851) is the record, and it names `receipt_id` in its `receipt` tag and the
amount that did arrive. A packet that is rejected is reported as `toon send` reports one, with
its exit code.
