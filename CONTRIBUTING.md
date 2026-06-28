# Contributing to obleth-gateway

Thanks for contributing. This project accepts bug reports, documentation fixes,
small targeted improvements, tests, and larger design changes discussed in advance.

## Before opening a pull request

- Search existing issues and pull requests first.
- Open an issue or discussion before large architectural changes.
- Keep changes focused. Avoid bundling unrelated refactors with a fix or feature.
- Update tests or docs when behavior changes.

## Development setup

### Rust workspace

The main Rust workspace lives in `obleth/`.

- Install stable Rust.
- Start local Postgres and Redis if your change touches integration tests.
- Set:
  - `OBLETH_TEST_DATABASE_URL=postgres://obleth:obleth@localhost:5432/obleth_test`
  - `OBLETH_TEST_REDIS_URL=redis://localhost:6379`

Common commands:

```sh
cd obleth
cargo clippy --workspace --all-targets
cargo test --workspace
```

### Control plane

The Next.js control plane lives in `control-plane/`.

```sh
cd control-plane
npm install --no-audit --no-fund
npm run build
```

### CI checks

CI currently runs:

- `bash scripts/check-versions.sh`
- `cargo clippy --workspace --all-targets`
- `cargo test --workspace`
- `cargo build --release` in `benchmark-backend/`
- `npm run build` in `control-plane/`

If your change affects one of those areas, run the closest matching check locally
before opening a PR.

## Pull request guidelines

- Explain the problem first, then the change.
- Include screenshots for control-plane UI changes.
- Call out migrations, config changes, or compatibility risks explicitly.
- Prefer small PRs with a clear scope.
- Be responsive to review comments and keep discussion attached to the code.

## Contributor certificate

This project uses a repository-specific contributor certificate rather than a
separate contributor agreement.

By opening a pull request, you certify that you have the right to submit the
work and that it may be distributed as part of this project under the terms in
[LICENSE](LICENSE), including the Change License that applies to the relevant
version.

Read the full text in [CONTRIBUTOR_CERTIFICATE.md](CONTRIBUTOR_CERTIFICATE.md).

A `Signed-off-by` trailer is not required. If you prefer to keep one for commit
provenance, you may still use `git commit -s`, but the pull request
acknowledgement is the required step for this repository.

## Licensing notes

This repository is source-available under the Business Source License 1.1. The
current license permits internal, academic, research, educational, and other
allowed uses described in [LICENSE](LICENSE), while restricting hosted resale and
competing commercial gateway products until the Change Date.

If you submit a contribution, you agree that your contribution will be licensed
under the terms described in [CONTRIBUTOR_CERTIFICATE.md](CONTRIBUTOR_CERTIFICATE.md)
and incorporated into the project under [LICENSE](LICENSE).