# Releasing obleth-gateway

Releases are **lockstep**: one `vX.Y.Z` git tag publishes everything together —
the five Docker images (`obleth`, `obleth-provisioner`, `control-plane`,
`benchmark-backend`, `obleth-compressor`; multi-arch, to
`ghcr.io/thediymaker/obleth-gateway/*`), the Helm chart (OCI, to
`ghcr.io/thediymaker/charts/obleth`), and a GitHub Release whose notes are the
version's `## vX.Y.Z` section of `CHANGELOG.md`. There are no separate
frontend/backend versions.

## Cutting a release

Work is integrated on `dev` (see Edge builds) and a release is cut from
`main`, so a release starts by promoting `dev`.

```bash
# 1. On dev: write the release's notes as a `## vX.Y.Z` section at the top of
#    CHANGELOG.md (the release job publishes that section as the GitHub
#    Release notes; without one it falls back to generated notes), then bump
#    the version everywhere it is declared (Cargo.toml x2, package.json,
#    Chart.yaml) and refresh the lockfiles. This is the only sanctioned way.
bash scripts/bump-version.sh 0.3.0
git add CHANGELOG.md obleth/Cargo.toml obleth/Cargo.lock \
  benchmark-backend/Cargo.toml benchmark-backend/Cargo.lock \
  control-plane/package.json deploy/k8s/obleth/Chart.yaml
git commit -m "release: v0.3.0"
git push origin dev

# 2. Promote dev to main (fast-forward) and tag the release there.
git checkout main && git merge --ff-only dev
git tag v0.3.0
git push origin main v0.3.0
```

```bash
# 3. Once the tag pipeline is green, move every running deployment to the
#    release. The chart's empty image tags resolve to v{appVersion}, so one
#    chart upgrade carries the compressor and every other component along;
#    --reset-then-reuse-values keeps your overrides (such as edge :dev tags)
#    and still picks up defaults the new chart changed, which plain
#    --reuse-values would pin at their old values. A release is not done
#    until this has run.
helm upgrade obleth oci://ghcr.io/thediymaker/charts/obleth \
  --version 0.3.0 --namespace obleth --reset-then-reuse-values
```

Pushing the tag triggers `.github/workflows/release.yml`, which:

1. **verify** — fails fast if the tag doesn't match the version in the files
   (`scripts/check-versions.sh`).
2. **build** — pushes per-arch tags for each component. arm64 builds
   natively on an arm64 runner (never QEMU). amd64 re-tags the `:dev` or
   `:main` edge image built from the tagged commit (it waits for an edge
   build of that commit still running, and checks the image's
   `org.opencontainers.image.revision` label against the tag's commit), and
   builds only when no edge image matches. Builds stamp the commit time as
   the build timestamp, so either way `/api/v1/version` reports the same.
3. **manifest** — merges them into multi-arch `:vX.Y.Z` and `:latest` tags.
4. **helm** — packages and pushes the chart to
   `oci://ghcr.io/thediymaker/charts/obleth`.
5. **release** — creates the GitHub Release, with the version's
   `CHANGELOG.md` section as its notes (generated notes only when there is
   no such section).

## Release candidates

Prerelease tags (`v0.3.0-rc.1`) run the same pipeline but never move
`:latest`, and the GitHub Release is marked as a prerelease (so the dashboard's
update check, which uses `releases/latest`, ignores it). Use one as a dry run
before the first stable tag of a version.

## Version surfaces

- Gateway: `GET /api/v1/version` (public) — version, git SHA, build timestamp.
- Dashboard: Settings → About (also compares against the latest GitHub
  release) and the user menu footer.
- CI guard: the `versions` job in `ci.yml` fails any PR where the five version
  declarations drift apart.

## Edge builds

Every push to `main` or `dev` publishes amd64-only images tagged after the
branch via `.github/workflows/docker.yml` — useful for testing unreleased
fixes (`OBLETH_VERSION=dev` in `deploy/docker/.env`). Features are integrated
on `dev`, so `:dev` is the moving edge; `:main` tracks what has been promoted
toward a release.

## First-release checklist (one-time)

- [ ] After the first publish, set each GHCR package to **public** (GitHub →
      profile → Packages → package → Package settings → Change visibility):
      `obleth-gateway/obleth`, `obleth-gateway/obleth-provisioner`,
      `obleth-gateway/control-plane`, `obleth-gateway/benchmark-backend`,
      `obleth-gateway/obleth-compressor`, and `charts/obleth`. They default to
      private and unauthenticated pulls 403 until flipped; an install with
      `provisioner.enabled` or `compressor.enabled` cannot pull a private one.
- [ ] Verify `docker pull ghcr.io/thediymaker/obleth-gateway/obleth:latest`
      works logged out.
- [ ] Verify the dashboard Settings → About shows the gateway as "Latest".
