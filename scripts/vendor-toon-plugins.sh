#!/usr/bin/env bash
# Replace vendor/toon-plugins/ with the toon-nostr and toon-cli plugins of one toon-plugins
# commit, and write that commit's full SHA to vendor/toon-plugins/REV (ADR 0013).
# The toon-client plugin describes another client and is never vendored.
#
#   scripts/vendor-toon-plugins.sh <40-character sha>
set -euo pipefail

sha="${1:-}"
if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
  echo "usage: $0 <full 40-character toon-plugins commit sha>" >&2
  exit 2
fi

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
repository="${TOON_PLUGINS_REPOSITORY:-https://github.com/toon-protocol/toon-plugins}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

git init -q "$work/src"
git -C "$work/src" fetch -q --depth 1 "$repository" "$sha"
git -C "$work/src" checkout -q FETCH_HEAD
if [[ "$(git -C "$work/src" rev-parse HEAD)" != "$sha" ]]; then
  echo "fetched a commit other than $sha" >&2
  exit 1
fi

target="$root/vendor/toon-plugins"
rm -rf "$target"
mkdir -p "$target/plugins"
for plugin in toon-nostr toon-cli; do
  cp -R "$work/src/plugins/$plugin" "$target/plugins/$plugin"
done
printf '%s\n' "$sha" > "$target/REV"
echo "vendored toon-plugins $sha"
