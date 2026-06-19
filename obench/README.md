# obench

`obench` is the obleth benchmark and readiness suite. It seeds a demo set of
models and tenants into the gateway, drives load against it, and exits with a
verdict: **PASS** means the deployment stayed up and served the load at the
configured concurrency. obench does not assert fairshare ratios or accounting
accuracy — those are things you observe. After every run it prints URLs pointing
at the fairshare dashboard and the accounting view in the control-plane UI so you
can inspect them directly.

## Prerequisites

A Rust toolchain via [rustup](https://rustup.rs). obench uses `rustls` for TLS
so OpenSSL does not need to be installed or linked.

## Build and run

```bash
# build the release binary from the repo root
cargo build --release --manifest-path obench/Cargo.toml

# the binary lands at obench/target/release/obench
./obench/target/release/obench --help
```

Run headless (both `--target` and `--profile` present → no TUI):

```bash
obench --target fixture --profile smoke --all
obench --target fixture --profile heavy --model obench-turbo
obench --target live   --profile auto  --all
```

Run the interactive TUI (omit one or both of `--target`/`--profile`, or pass
`--no-tui` to force headless without picking both on the command line):

```bash
obench
```

Connection defaults (`--admin-base`, `--proxy-base`, `--admin-token`,
`--ui-base`) match the local docker-compose stack and can also be set via
environment variables `ADMIN_BASE`, `ADMIN_TOKEN`, `PROXY_BASE`, `UI_BASE`.

## Target × Profile × Scope

Every run is described by three dimensions:

| Dimension | Values | Meaning |
|-----------|--------|---------|
| **Target** | `fixture`, `live` | Where to send requests. `fixture` uses the GPU-free `benchmark-backend` container. `live` uses real upstream APIs listed in a config file. |
| **Profile** | `smoke`, `light`, `heavy`, `extreme`, `auto`, `manual` | Load intensity and duration. See below. |
| **Scope** | `--all` (default), `--model <name>` | Drive the full demo fleet or a single named model. |

**Validity constraint:** `--target live --profile extreme` is blocked. `extreme`
measures the gateway's raw req/s ceiling using tiny 4-token outputs against the
GPU-free fixture backend, where generation time is negligible. Against live
upstreams the generation time dominates and the number is not meaningful. Use
`--target fixture` for extreme, or pick `auto` or `heavy` for live.

### Profiles

| Profile | Concurrency | Duration | Output tokens | Purpose |
|---------|-------------|----------|---------------|---------|
| `smoke` | 2 | 30 s | 16 | Check the stack responds at all |
| `light` | 16 | 60 s | 64 | Routine CI / sanity check |
| `heavy` | 64 | 600 s | 128 | Sustained realistic load |
| `extreme` | 256 | 30 s | 4 | Max req/s ceiling (fixture only) |
| `auto` | ramp | auto | 4 | Self-calibrating (see below) |
| `manual` | 64 | 60 s | 64 | Fully overridden by CLI flags |

Per-profile defaults can be overridden with `--conc`, `--duration-s`,
`--output-tokens`, `--capacity`, and `--max-error-rate`.

## The `auto` profile

`auto` runs a stepped concurrency ramp (`32 → 64 → 128 → 256 → 512 → 1024 →
2048`), holding each step for 12 seconds after a 2-second warmup. At each step
it records throughput, error rate, and p99 TTFB. A knee detector watches for the
point where req/s stops growing cleanly — rising error rate, collapsing
throughput, or latency runaway — and stops there.

The **sustainable concurrency** (the last clean step) is reported at the end and
written into `auto-meta.json` together with a `replay` block:

```json
{
  "sustainable_conc": 256,
  "replay": { "profile": "manual", "conc": 256, "output_tokens": 4, "stream": false }
}
```

To reproduce the found ceiling, pass the `replay` values as CLI flags:

```bash
obench --target fixture --profile manual --conc 256 --output-tokens 4 --all
```

## The `obench-` demo set (idempotent seeding)

For `--target fixture`, obench seeds the gateway before every run with a
canonical set of models, fairshare groups, and tenants whose names all start with
`obench-`. This prefix is the identity contract:

- **Models** (`obench-turbo`, `obench-base`, `obench-code`, `obench-large`,
  `obench-embed`) are registered against the fixture backend with the correct
  upstream URL. If they already exist, obench updates them in place — it does
  not create duplicates.
- **Tenants** and **fairshare groups** follow the same upsert logic: if a
  tenant named `obench-chatbot` already exists, the run reuses it.
- **API keys** are cached in `$BENCH_OUT_DIR/keys.json` (written 0600 on Unix,
  directory tightened to 0700). On the next run obench calls `ensure_key` again:
  if the gateway mints a new secret (because the key was deleted), the cache is
  updated; if the key was reused, the cached secret is read back. Stale keys with
  the same name are pruned so there is no test-key sprawl across runs.

Nothing generated is written into the source directory.

## Live config

For `--target live`, obench reads a JSON config file (default `live.config.json`,
override with `--config <path>`). The file lists the real upstream models and the
clients (tenants) to create:

```json
{
  "models": [
    {
      "name": "gpt-4o",
      "upstream_model": "gpt-4o",
      "api_base": "https://api.openai.com/v1",
      "api_key": "${OPENAI_API_KEY}",
      "weight": 100,
      "input_cost_per_token": 0.0000025,
      "output_cost_per_token": 0.00001
    }
  ],
  "clients": [
    { "name": "bench-primary", "group": "api-batch", "weight": 100 },
    { "name": "bench-secondary", "group": "chatbot",  "weight": 500 }
  ]
}
```

Any value may contain `${VAR}` placeholders. obench expands them from the
environment at load time. A missing variable is a **hard error** — it is never
silently replaced with an empty string. This prevents accidental runs with blank
API keys.

**Safety warning:** live runs send real requests to real upstream APIs using
real keys. Every completion incurs cost. The `keys.json` cache in
`$BENCH_OUT_DIR` holds real gateway secrets; protect that directory accordingly
(it is written 0600 on Unix). Set `BENCH_OUT_DIR` to a path outside the repo if
the default `/tmp/obleth-bench` is not suitable for your environment.

`--all` requires at least 2 models and 2 clients in the config
so that load genuinely spreads across upstreams and distinct tenants.
`--model <name>` requires that the named model appear in `models[]`.

## Artifacts

All output goes to `BENCH_OUT_DIR` (default `/tmp/obleth-bench`). Files written
per run:

| File | Written by | Contents |
|------|-----------|---------|
| `<profile>-meta.json` | every profile | target, profile, scope, completions, req/s, error rate, p50/p99 TTFB, token counts, verdict |
| `<profile>-timeline.jsonl` | headless fixed-load runs | per-10-second rows: `in_flight`, `queued` |
| `auto-meta.json` | `auto` profile | above + `sustainable_conc`, step history, `replay` block |
| `keys.json` | seeding | tenant name → gateway API secret cache (written 0600) |

After every run, obench prints a summary and two control-plane URLs:

```
verdict: PASS — deployment stayed up and served the load
requests: 4201 ok / 4215 attempts  (70 req/s)
errors: 0.33%   429: 0
ttfb ms: p50=42  p90=88  p99=140
tokens: in 512000 out 268864
watch in the control plane:
  fairshare   http://localhost:3000/fairshare
  accounting  http://localhost:3000/usage
```

Fairshare ratios, per-tenant accounting, and ledger reconciliation are things you
observe in those views — obench does not assert them automatically.
