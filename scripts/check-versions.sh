#!/usr/bin/env bash
# Assert the lockstep version is identical in every file that declares it.
#
# Usage:
#   scripts/check-versions.sh           # consistency check (CI, pre-commit)
#   scripts/check-versions.sh v0.2.0    # additionally assert the release tag matches
set -euo pipefail
cd "$(dirname "$0")/.."

workspace=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' obleth/Cargo.toml)
benchmark=$(sed -n '/^\[package\]/,/^\[/s/^version = "\(.*\)"/\1/p' benchmark-backend/Cargo.toml)
control_plane=$(sed -n 's/^[[:space:]]*"version":[[:space:]]*"\([^"]*\)".*/\1/p' control-plane/package.json | head -n1)
chart=$(sed -n 's/^version: //p' deploy/k8s/obleth/Chart.yaml)
chart_app=$(sed -n 's/^appVersion: "\(.*\)"/\1/p' deploy/k8s/obleth/Chart.yaml)

echo "obleth/Cargo.toml            $workspace"
echo "benchmark-backend/Cargo.toml $benchmark"
echo "control-plane/package.json   $control_plane"
echo "Chart.yaml version           $chart"
echo "Chart.yaml appVersion        $chart_app"

if [ -z "$workspace" ]; then
    echo "error: could not extract version from obleth/Cargo.toml" >&2
    exit 1
fi

ok=1
for v in "$benchmark" "$control_plane" "$chart" "$chart_app"; do
    [ "$v" = "$workspace" ] || ok=0
done
if [ "$ok" -ne 1 ]; then
    echo "error: version mismatch — run scripts/bump-version.sh <X.Y.Z> to sync" >&2
    exit 1
fi

if [ $# -ge 1 ]; then
    tag="$1"
    # Prerelease tags (v0.2.0-rc.1) only need the base version to match.
    if [ "${tag%%-*}" != "v$workspace" ]; then
        echo "error: tag '$tag' does not match project version 'v$workspace'" >&2
        echo "bump the version (scripts/bump-version.sh) before tagging" >&2
        exit 1
    fi
fi

echo "ok: all versions are $workspace"
