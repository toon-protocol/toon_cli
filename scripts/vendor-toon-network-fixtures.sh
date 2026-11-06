#!/usr/bin/env bash
# Replace vendor/toon-network-fixtures/ with Appendix B's wire fixtures of one TOON_Network
# commit (docs/spec/fixtures/wire/), and write that commit's full SHA to
# vendor/toon-network-fixtures/REV, as scripts/vendor-toon-plugins.sh does for the skills
# (ADR 0013). The tests load these files and build their requests to match them.
#
#   scripts/vendor-toon-network-fixtures.sh <40-character sha>
set -euo pipefail

sha="${1:-}"
if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: $0 <full 40-character TOON_Network commit sha>" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repository="${TOON_NETWORK_REPOSITORY:-https://github.com/toon-protocol/TOON_Network}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

git init -q "$work/src"
git -C "$work/src" fetch -q --depth 1 "$repository" "$sha"
git -C "$work/src" checkout -q FETCH_HEAD
if [[ "$(git -C "$work/src" rev-parse HEAD)" != "$sha" ]]; then
  echo "fetched a commit other than $sha" >&2
  exit 1
fi

target="$root/vendor/toon-network-fixtures"
rm -rf "$target"
mkdir -p "$target"
cp "$work/src/docs/spec/fixtures/wire/"* "$target/"
printf '%s\n' "$sha" > "$target/REV"
echo "vendored TOON_Network fixtures $sha"
