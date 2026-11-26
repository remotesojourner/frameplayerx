#!/usr/bin/env bash
# What's New: each change in crates/frameplayerx/whats-new/ and the version
# that first shipped it, "<name> <version>" a line, oldest first, for the
# build's FP_WHATS_NEW. That version is the earliest release tag holding
# the commit that added the change's file; a change no tag holds yet ships
# in VERSION, the one being built.
#
# Usage: tools/whats-new.sh VERSION   (needs the whole history and its tags)
set -euo pipefail
cd "$(dirname "$0")/.."
version="$1"
if [ "$(git rev-parse --is-shallow-repository)" = true ]; then
  echo "whats-new: a shallow clone can't tell which version shipped a change" >&2
  exit 1
fi
for f in crates/frameplayerx/whats-new/*.json; do
  name="$(basename "$f" .json)"
  added="$(git log --diff-filter=A --format='%H %ct' -- "$f" | tail -n 1)"
  if [ -z "$added" ]; then
    # Not committed yet.
    echo "9999999999 $name $version"
    continue
  fi
  # Betas sort before their release.
  tag="$(git -c versionsort.suffix=- tag --contains "${added% *}" --list 'v[0-9]*' --sort=v:refname | sed -n 1p)"
  echo "${added#* } $name ${tag#v}"
done | sort -n -k1,1 -k2,2 | awk -v version="$version" '{ print $2, ($3 == "" ? version : $3) }'
