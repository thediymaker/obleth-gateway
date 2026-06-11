#!/usr/bin/env bash
# Bump the lockstep project version everywhere it is declared, then refresh
# both Cargo.lock files. This is the only sanctioned way to change the version.
#
# Usage: scripts/bump-version.sh 0.2.0
set -euo pipefail
cd "$(dirname "$0")/.."

new="${1:-}"
if ! printf '%s' "$new" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$'; then
    echo "usage: scripts/bump-version.sh X.Y.Z" >&2
    exit 1
fi

sed -i "/^\[workspace\.package\]/,/^\[/s/^version = \".*\"/version = \"$new\"/" obleth/Cargo.toml
sed -i "/^\[package\]/,/^\[/s/^version = \".*\"/version = \"$new\"/" benchmark-backend/Cargo.toml
sed -i "0,/\"version\":/s/\"version\":[[:space:]]*\"[^\"]*\"/\"version\": \"$new\"/" control-plane/package.json
sed -i "s/^version: .*/version: $new/" deploy/k8s/obleth/Chart.yaml
sed -i "s/^appVersion: .*/appVersion: \"$new\"/" deploy/k8s/obleth/Chart.yaml

# Refresh lockfiles so --locked builds keep working.
(cd obleth && cargo check --quiet)
(cd benchmark-backend && cargo check --quiet)

scripts/check-versions.sh
echo "bumped to $new — commit, then tag with: git tag v$new && git push origin main v$new"
