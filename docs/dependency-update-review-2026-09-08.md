# Dependency update review — 2026-09-08

This is a source and dependency refresh, not a deployment. Existing running containers were left in place. Application release versions remain 0.9.6.

## Applied

- Refreshed npm dependencies within compatible release lines and raised manifest minimum versions to the resolved versions. Next.js is 16.3.4; React/React DOM are 19.2.8; Vitest is 4.1.11.
- Better Auth is constrained to `~1.6.30`. The 1.7 release removes the generic OAuth client API and changes callback URLs. The 1.7.3 account schema is unchanged from 1.6; callback registration and provider behavior still require a coordinated migration. See the [official upgrade guide](https://better-auth.com/docs/guides/1-7-upgrade-guide).
- Dashboard Docker stages and CI now use Node 24 LTS, with matching Node 24 types. The host Node installation was not changed. [Node release lifecycle](https://nodejs.org/en/about/previous-releases).
- Refreshed all three Cargo lockfiles. Upgraded OpenTelemetry SDK to 0.32.1, its companion crates, Prometheus to 0.14, and Ratatui/Crossterm to 0.30.2/0.29. These remove the reported baggage parsing, protobuf recursion, and LRU soundness advisories.
- Compose now uses HAProxy 3.2 instead of the EOL 2.9 branch. Configuration validation passed in a disposable container; the unresolved `obleth` hostname notice is expected outside the Compose network. [HAProxy support status](https://docs.haproxy.org/).

## Audit results

- npm audit: **0 known vulnerabilities**, down from 11 (1 critical, 6 high, 3 moderate, 1 low).
- OSV batch scan of updated Cargo lockfiles: no findings for obench or benchmark-backend.
- Gateway lockfile has two remaining findings: [proc-macro-error is unmaintained](https://osv.dev/vulnerability/RUSTSEC-2024-0370), inherited through the older OpenAPI generation dependency; and [RSA timing side channels](https://osv.dev/vulnerability/RUSTSEC-2023-0071). `cargo tree -i rsa --locked` reports no enabled dependency path in the current build. The lockfile finding is retained for review if SQLx features change; there is no released patch identified by the advisory.
- Compressor requirements already allow the current PyPI releases of FastAPI, Uvicorn, ONNX Runtime, tokenizers, NumPy, and Pydantic. No installed compressor environment or model inference behavior was upgraded or validated in this pass.

## Still requires migration or deployment work

- Better Auth 1.7 OAuth callback/client migration and provider-specific sign-in checks.
- Major UI/toolchain migrations including Tailwind 4, Recharts 3, Zod 4, TypeScript 7, Vitest 5, and other packages outside existing version ranges. The Recharts 2 install currently emits an inactive-branch warning.
- Older Rust API release lines, especially OpenAPI generation, should be upgraded with their generated schema reviewed.
- Stateful infrastructure remains on the existing Postgres 16, Redis 7, and ClickHouse 24.8 defaults. Dex 2.41.1 and Jaeger 1.60 also remain pinned. Plan and validate these upgrades separately; do not retag existing data volumes blindly. See [ClickHouse upgrade guidance](https://clickhouse.com/docs/guides/oss/update).
- Floating `latest` observability image tags and broad Python ranges do not prove that running containers are current. Rebuild, scan the resulting images, and roll out verified artifacts.
- Release/publish/deploy has not been performed. Neither the application version nor existing running containers were changed.

## Validation

- Dashboard: 358 tests pass; TypeScript passes.
- Gateway: formatting, Clippy across all targets, and workspace tests pass with isolated Postgres database `obleth_test_updates_20260908` and Redis database 13 configured.
- obench: 135 tests and release build pass after the terminal UI upgrade.
- benchmark-backend: 7 tests and release build pass.
- Compose configuration and five-way release-version consistency pass.
- Node 24 production image build is recorded separately at completion.

This report does not claim the entire deployed application is on every latest major version or free of all possible vulnerabilities.
