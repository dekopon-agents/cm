#!/usr/bin/env bash
set -euo pipefail
declared=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$1" | head -n 1)
if [[ ! $declared =~ ^([0-9]+)\.([0-9]+)\.0$ ]]; then
  echo "Cargo.toml version '$declared' must be X.Y.0; CI picks the patch" >&2
  exit 1
fi
line="${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
newest=-1
for tag in $2; do
  if [[ $tag =~ ^v${line//./\\.}\.([0-9]+)$ ]] && (( BASH_REMATCH[1] > newest )); then
    newest=${BASH_REMATCH[1]}
  fi
done
echo "$line.$((newest + 1))"
