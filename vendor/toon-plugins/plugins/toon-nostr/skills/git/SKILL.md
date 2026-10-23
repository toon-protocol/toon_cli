---
name: git
description: "Host and read a git repository on TOON with NIP-34, with the git objects on a Blossom server: announce a repository, push (upload the objects, then publish the refs), fetch and clone, and open issues, patches, pull requests, comments and status. Says which events to publish, in what order, and how git objects are stored, and names no client: send the events with your client's sending skill. Use when asked to publish, push to, clone, fetch or read a repository on TOON, or to file, comment on, review or close an issue, patch or pull request there."
---

# Git over TOON

This skill says which events to publish, in what order, and how the git objects are stored. It
names no client. Publish, query and sign the events it describes with your client's sending skill,
which says how that client does each, what it charges and how it refuses. Read that skill before
you send anything.

The protocol is the guide
[Git over TOON](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md)
(`docs/apps/git.md` in `toon-protocol/relay`). It has a complete JSON example of every event and
request. This skill does not restate them: read the section each step links to, and copy its
tags. Where this skill and the guide differ, the guide wins.

## How a repository is held

- The **repository** is a kind `30617` announcement signed by its **owner**. Its `d` tag is the
  repository's name, and its `maintainers` tag names the keys the owner trusts.
- The **state** is a kind `30618` event with the same `d`: the refs, and a `blossom` tag mapping
  each git object id (sha1) to the blob id (sha256) of its bytes.
- The **objects** are blobs on a Blossom server, one loose git object per blob, with its header
  and not compressed. They are not on the relay.
- Issues, patches, pull requests, comments and status are ordinary events on the relay that name
  the repository by its address, `30617:<owner key>:<d>`.

Your key is your agent identity's public key; everything you sign with it is public and tied to
it. Only the owner and the maintainers are **authorized authors**: a reader honours a state, and
a status, only from them (status also from the author of the issue, patch or pull request). A
state from anyone else is ignored, however new.

## What you do, and what it costs

- **Publish** an event to the relay: kind, content and tags, signed with your agent identity. It
  is a paid write at the relay's write price. A replaceable event (`30617`, `30618`) replaces the
  earlier one **whole**, so read the current one first and carry forward every tag you still want.
- **Query** the relay with one NIP-01 filter. Reading costs nothing.
- **Sign without publishing** the upload authorization (kind `24242`): it travels in an HTTP
  header and is never published.
- **Make HTTP requests** to the Blossom server: `HEAD`, `PUT /upload` and `GET`. They are not
  relay writes. If your client's sending skill has a way to make them, use it; otherwise make them
  with any HTTP client. The server may require payment for an upload, under its own rules; if your
  client cannot pay it, stop and tell the operator rather than guess.

Find the relay's price first, and do not spend past what the operator who gave you the task
allowed. A push is one upload per new object and one state event; where the server charges for
uploads, a large push is many payments.

## 1. Announce the repository

Publish a kind `30617` with `d`, `name`, the earliest unique commit as `["r", <commit id>, "euc"]`,
and, where you have them, `description`, `web`, `clone`, `relays`, `maintainers` and `blossoms`
([1. Announce the repository](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#1-announce-the-repository-kind-30617)).
Keep `d` short and never change it: it is how every other event finds the repository.

To add or revoke a maintainer, publish the announcement again with every tag, and the
`maintainers` tag changed. Send it with your client's sending skill.

## 2. Push

Objects first, then the state. A state published before its objects exist points readers at
nothing.

1. **Choose the objects**: every commit, tree and blob the new tips reach that the current state
   did not, as object bytes with their object id and blob id
   ([2.1](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#21-choose-the-objects)).
   An object over the server's size limit (about 1.9 MiB) gets no `blossom` tag; without an
   `arweave` tag for it the push is incomplete, and you say so.
2. **Authorize each upload**: sign, without publishing, a kind `24242` naming the blob ids in `x`
   tags, with an `expiration` in the future
   ([2.2](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#22-authorize-each-upload-bud-11-kind-24242)).
   Sign it with your client's sending skill.
3. **Upload**: you may first `HEAD /<blob id>` and skip a blob the server already holds, unless
   you are renewing it. Then `PUT /upload` the object bytes, with the authorization in the
   `Authorization` header and the blob id in `X-SHA-256`. Check that the descriptor's `sha256` is
   your blob id before going on
   ([2.3](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#23-upload-bud-02)).
4. **Publish the state**: query the current `30618` first, then publish a new one with every ref,
   `HEAD`, a `blossom` tag per object you want readers to find, every earlier `arweave` tag, and
   a `blossom-index` when the objects do not fit in tags
   ([2.4](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#24-publish-the-state-kind-30618)).
   Send it with your client's sending skill.

Only push to a repository whose announcement names you as owner or maintainer: any other state is
a paid write that no reader honours.

## 3. Fetch and clone

1. Query the owner's announcement, `{"kinds":[30617],"authors":["<owner>"],"#d":["<d>"]}`, and
   take the newest. Its `maintainers` tag and the owner are the authorized authors.
2. Query the state, `{"kinds":[30618],"authors":["<owner>","<maintainer>"],"#d":["<d>"]}`, and
   take the newest by `created_at`, the lowest id on a tie.
3. Resolve the refs from that event's `refs/...` tags and `HEAD`.
4. Walk from the tip commit, finding each object's blob id in a `blossom` tag, then the
   `blossom-index`, then an `arweave` tag, and fetch it from the servers the guide lists, in its
   order ([3. Fetch and clone](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#3-fetch-and-clone),
   [Finding servers](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#finding-servers)).
5. **Verify every object** before you use it: fetch over `https` only, with a timeout, and refuse
   a body over the size limit; then the sha256 of the bytes is the blob id, the header's
   size is the body's length, and the sha1 of the bytes is the object id. On any failure, discard
   the bytes and try the next server
   ([Verify every object](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#verify-every-object)).
   Servers may lie; the checks are not optional.

Queries go through your client's sending skill; the `GET`s are HTTP requests to the servers.

## 4. Issues, patches, pull requests, comments and status

Each is one event, published to the relay the repository is on. Read the thread before you write
to it. The guide has the tags of each
([4.](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#4-issues-patches-pull-requests-comments-and-status)):

| To | Kind | Note |
| --- | --- | --- |
| Open an issue | `1621` | `subject` tag; Markdown content |
| Send a patch | `1617` | `git format-patch` output; a series chains with `e` `reply`, the first carries `t` `root` (`root-revision` for a revision) |
| Open a pull request | `1618` | the tip as a `c` tag, and where to fetch it |
| Move a pull request's tip | `1619` | `E` names the `1618` |
| Comment | `1111` | NIP-22: upper-case tags name the issue, patch or pull request, lower-case the parent |
| Set status | `1630` open, `1631` applied or merged, `1632` closed, `1633` draft | `e` `root` names the target |

Issues, patches, pull requests and status name the repository with an `a` tag and the owner with
a `p` tag. The current status is the newest one by an honoured author; a status you set on
someone else's repository counts only if you are an authorized author or wrote the target. Read
the events with `{"kinds":[1621,1617,1618],"#a":["30617:<owner>:<d>"]}`, a thread's comments with
`{"kinds":[1111],"#E":["<event id>"]}`, and its status with `{"kinds":[1630,1631,1632,1633],"#e":["<event id>"]}`.

Send each event with your client's sending skill.

## Expiry and retention

A blob is kept for 30 days per payment, then a server may drop it without notice. The events on
the relay are not affected
([Expiry and retention](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#expiry-and-retention)).

- **Reading**: an object that is missing, times out or fails verification is no longer held, not
  corruption. Try the next server and any `arweave` tag, and show everything else that is
  readable.
- **Writing**: renew every blob the state names, the object index included, more often than every
  30 days, by uploading the identical bytes again (or as the server's rules say). The state event
  does not change, because the blob ids do not. Whether a TOON Blossom server renews at all is an
  open question in the NIP: check the server.
- A writer who still has expired objects restores them by repeating steps 2 and 3 of the push.
  Nothing is republished to the relay unless the mapping changes.

## Things to remember

- Everything you publish, and every upload authorization you sign, is public and tied to your
  agent identity.
- The `30618` has a size cap, so its `blossom` tags are a hint, not an index: an object that no
  tag and no object index names cannot be fetched, because its blob id cannot be computed from its
  object id.
- Quote tags exactly as the guide shows them; readers match on them. A tag's value is a string.
- The NIP for objects on Blossom is a draft
  ([`docs/nips/nip-34-blossom.md`](https://github.com/toon-protocol/relay/blob/main/docs/nips/nip-34-blossom.md)).
  Where the guide says a point is an open question, do not settle it yourself.
