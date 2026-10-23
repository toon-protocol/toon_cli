# Recipes: git objects to blobs, and back

Two tested shell recipes for the git guide's object rules. They need `git`, `jq`, `curl`,
`sha1sum` and `sha256sum`, and name no TOON client: uploading and querying are your client's
sending skill's.

## Read: the announcement and the state

Query with your client's sending skill and save what comes back, one event a line: the owner's
announcements in `announcements.jsonl`, then the states in `states.jsonl`. This picks the newest
of each, taking the lowest id on a tie (a plain `sort_by(.created_at) | last` takes the highest),
and reads the authorized authors, the server, the default branch and its tip:

```
newest='sort_by(.created_at, (.id | explode | map(-.))) | last'
jq -s "$newest" announcements.jsonl > announcement.json
jq -c '[.pubkey] + [.tags[] | select(.[0] == "maintainers") | .[1:][]] | unique' announcement.json
# query {"kinds":[30618],"authors":<that list>,"#d":["<d>"]} into states.jsonl, then:
jq -s "$newest" states.jsonl > state.json
server=$(jq -r '[.tags[] | select(.[0] == "blossoms") | .[1:][]][0] // empty' announcement.json)
branch=$(jq -r '.tags[] | select(.[0] == "HEAD") | .[1] | sub("^ref: "; "")' state.json)
tip=$(jq -r --arg b "$branch" '.tags[] | select(.[0] == $b) | .[1]' state.json)
```

With no state yet (a first push), `state.json` is `null`. `server` is the first of the
announcement's servers; the guide's [Finding servers](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#finding-servers)
gives the full order.

## Push: the bytes of each object

A blob is one loose object **with its header and not compressed**: `<type> <size>\0<body>`. The
files under `.git/objects` are zlib-compressed: never upload them as they are. This writes each
object the new tip reaches, as `$out/<object id>`, checks that the sha1 of the bytes is the
object id, and prints `<object id> <blob id>` for the `blossom` tags:

```
out=<directory for the object files>; new=<new tip, such as main>
old=$(jq -r '[.tags[]? | select(.[0] | startswith("refs/")) | .[1]] | join(" ")' state.json)
git rev-list --objects "$new" ${old:+--not $old} | cut -d' ' -f1 | while read -r oid; do
  type=$(git cat-file -t "$oid")
  { printf '%s %d\0' "$type" "$(git cat-file -s "$oid")"; git cat-file "$type" "$oid"; } > "$out/$oid"
  [ "$(sha1sum < "$out/$oid" | cut -c1-40)" = "$oid" ] || { echo "bad bytes for $oid"; break; }
  echo "$oid $(sha256sum < "$out/$oid" | cut -c1-64)"
done
```

`--not` the state's current tips leaves out what the repository already holds (the guide's
2.1). Each line then gives one upload: the file is the body, and the blob id is the `x` tag
and the `X-SHA-256` header. Save the lines as `map.txt`.

## Push: the new state

Once every upload has succeeded, this builds the unsigned kind `30618` in `new-state.json`. It
sets the branch's tip, keeps every other ref, adds a `blossom` tag for each line of `map.txt`,
and carries forward every earlier `blossom`, `arweave` and `blossom-index` tag, as the guide's
[2.4](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#24-publish-the-state-kind-30618)
says. Dropping one silently hides an object from every reader:

```
d=<d>; branch=refs/heads/<name>; tip=$(git rev-parse <name>)
jq -n --slurpfile cur state.json --rawfile map map.txt --arg d "$d" --arg branch "$branch" --arg tip "$tip" '
  ($cur[0].tags // []) as $old
  | {kind: 30618, content: "", tags: (
      [["d", $d], [$branch, $tip]]
      + [$old[] | select((.[0] | startswith("refs/")) and .[0] != $branch)]
      + [["HEAD", ("ref: " + $branch)]]
      + ([$old[] | select(.[0] == "blossom")]
         + ($map | split("\n") | map(select(. != "") | split(" ") | ["blossom", .[0], .[1]]))
         | unique_by(.[1]))
      + [$old[] | select(.[0] == "arweave" or .[0] == "blossom-index")])}' > new-state.json
```

Sign and publish it with your client's sending skill.

## Fetch and clone: verify, store, walk

This rebuilds a repository from the state alone, with `state.json`, `$server`, `$branch` and
`$tip` from "Read" above. It fetches each object over
`https` with a timeout and a size cap, checks the sha256 against the blob id, the header's size
against the body and the sha1 against the object id, then stores it with `git hash-object`.
Git itself lists what a stored commit or tree points to, so nothing parses a tree by hand.
Submodule entries (mode `160000`) are not objects of this repository and are skipped.

```
state=$PWD/state.json  # and $server, $branch, $tip from "Read" above
git init -q repo && cd repo
get() {  # get <object id>: fetch, verify, and store one object
  local oid=$1 bid hdr
  bid=$(jq -r --arg o "$oid" '.tags[] | select(.[0] == "blossom" and .[1] == $o) | .[2]' "$state")
  [ -n "$bid" ] || { echo "no blob id for $oid"; return 1; }
  curl -sf --proto =https --max-time 30 --max-filesize 2000000 "$server/$bid" -o ../obj || return 1
  hdr=$(head -c 32 ../obj | tr '\0' '\n' | head -n1)
  [ "$(sha256sum < ../obj | cut -c1-64)" = "$bid" ] && [ "$(sha1sum < ../obj | cut -c1-40)" = "$oid" ] \
    && [ $(( $(stat -c %s ../obj) - ${#hdr} - 1 )) = "${hdr#* }" ] || { echo "bad bytes for $oid"; return 1; }
  [ "$(tail -c +$(( ${#hdr} + 2 )) ../obj | git hash-object -w -t "${hdr% *}" --stdin)" = "$oid" ]
}
todo=($tip)
while [ ${#todo[@]} -gt 0 ]; do
  oid=${todo[0]}; todo=("${todo[@]:1}")
  git cat-file -e "$oid" 2>/dev/null && continue
  get "$oid" || exit 1
  case $(git cat-file -t "$oid") in
    commit) todo+=($(git cat-file -p "$oid" | sed -n '/^$/q; s/^tree //p; s/^parent //p')) ;;
    tree)   todo+=($(git ls-tree "$oid" | awk '$1 != "160000" { print $3 }')) ;;
  esac
done
git update-ref "$branch" "$tip" && git symbolic-ref HEAD "$branch" && git checkout -q -f && git fsck --strict
```

It looks a blob id up in the `blossom` tags only, and tries one server. When an object has no
tag there, look in the `blossom-index`, then an `arweave` tag, and try the next server on any
failure, as the guide's [3. Fetch and clone](https://github.com/toon-protocol/relay/blob/main/docs/apps/git.md#3-fetch-and-clone)
says.
