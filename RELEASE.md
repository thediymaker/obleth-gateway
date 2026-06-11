# Releasing obleth-gateway

Releases are **lockstep**: one `vX.Y.Z` git tag publishes everything together —
the `obleth`, `control-plane`, and `benchmark-backend` Docker images
(multi-arch, to `ghcr.io/thediymaker/obleth-gateway/*`), the Helm chart (OCI, to
`ghcr.io/thediymaker/charts/obleth`), and a GitHub Release with auto-generated
notes. There are no separate frontend/backend versions.

## Cutting a release

```bash
# 1. Bump the version everywhere it is declared (Cargo.toml x2, package.json,
#    Chart.yaml) and refresh the lockfiles. This is the only sanctioned way.
bash scripts/bump-version.sh 0.3.0

# 2. Commit and tag.
git add -A
git commit -m "release: v0.3.0"
git tag v0.3.0
git push origin main v0.3.0
```

Pushing the tag triggers `.github/workflows/release.yml`, which:

1. **verify** — fails fast if the tag doesn't match the version in the files
   (`scripts/check-versions.sh`).
2. **build** — builds each component natively on amd64 and arm64 runners
   (never QEMU) and pushes per-arch tags.
3. **manifest** — merges them into multi-arch `:vX.Y.Z` and `:latest` tags.
4. **helm** — packages and pushes the chart to
   `oci://ghcr.io/thediymaker/charts/obleth`.
5. **release** — creates the GitHub Release with generated notes.

## Release candidates

Prerelease tags (`v0.3.0-rc.1`) run the same pipeline but never move
`:latest`, and the GitHub Release is marked as a prerelease (so the dashboard's
update check, which uses `releases/latest`, ignores it). Use one as a dry run
before the first stable tag of a version.

## Version surfaces

- Gateway: `GET /api/v1/version` (public) — version, git SHA, build timestamp.
- Dashboard: Settings → Version card (also compares against the latest GitHub
  release) and the user menu footer.
- CI guard: the `versions` job in `ci.yml` fails any PR where the five version
  declarations drift apart.

## Edge builds

Every push to `main` publishes amd64-only `:main` tags via
`.github/workflows/docker.yml` — useful for testing unreleased fixes
(`OBLETH_VERSION=main` in `deploy/docker/.env`).

## First-release checklist (one-time)

- [ ] After the first publish, set each GHCR package to **public** (GitHub →
      profile → Packages → package → Package settings → Change visibility):
      `obleth-gateway/obleth`, `obleth-gateway/control-plane`,
      `obleth-gateway/benchmark-backend`, and `charts/obleth`. They default to
      private and unauthenticated pulls 403 until flipped.
- [ ] Verify `docker pull ghcr.io/thediymaker/obleth-gateway/obleth:latest`
      works logged out.
- [ ] Verify the dashboard Settings → Version card reports "Up to date".
