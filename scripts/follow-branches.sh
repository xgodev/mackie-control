#!/usr/bin/env bash
# Responsibility: moves the git dependencies declared with `branch =` to their branch tip.
#
# Sourced by scripts/update-libs.sh. Cargo.lock pins a branch dependency to
# the commit it first fetched; this asks each repository for its branch tip
# (`git ls-remote`, once per repository) and runs `cargo update -p` only on
# the packages whose tip moved, so each build picks the branch's latest commit
# and the next commit carries the bumped lock, without paying a full
# `cargo update` round trip when nothing moved. A tip that cannot be asked or
# a failed update (offline, no access) warns and the build goes on with the
# pinned commit.

follow_branches() {
  [ -f Cargo.lock ] || return 0
  local deps
  deps=$(awk '
    /^name = / { name = $3; gsub(/"/, "", name) }
    /^source = "git\+[^"]*\?branch=/ {
      src = $3; gsub(/"/, "", src); sub(/^git\+/, "", src)
      url = src; sub(/\?.*/, "", url)
      branch = src; sub(/.*\?branch=/, "", branch); sub(/#.*/, "", branch); gsub(/%2F/, "/", branch)
      pin = src; sub(/.*#/, "", pin)
      print url, branch, pin, name
    }
  ' Cargo.lock | sort -u)
  [ -n "$deps" ] || return 0
  local moved="" unknown="" url branch pin name asked="" tip=""
  while read -r url branch pin name; do
    if [ "$url $branch" != "$asked" ]; then
      asked="$url $branch"
      tip=$(git ls-remote "$url" "refs/heads/$branch" 2>/dev/null | awk '{ print $1; exit }') || tip=""
    fi
    if [ -z "$tip" ]; then
      unknown="$unknown$name "
    elif [ "$tip" != "$pin" ]; then
      moved="$moved$name "
    fi
  done <<<"$deps"
  [ -z "$unknown" ] ||
    echo "warning: could not ask the branch tip of ${unknown}; building the pinned commit" >&2
  [ -n "$moved" ] || return 0
  local args=()
  for name in $moved; do args+=(-p "$name"); done
  cargo update "${args[@]}" ||
    echo "warning: could not move ${moved}to the branch tip; building the pinned commit" >&2
}
