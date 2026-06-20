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
obench --target demo --profile smoke --all
obench --target demo --profile heavy --model obench-turbo
obench --target live   --profile auto  --all
```

Run the interactive TUI (omit one or both of `--target`/`--profile`, or pass
`--no-tui` to force headless without picking both on the command line):

```bash
obench
```

The TUI is a guided wizard — you don't need to pass any flags or write a config
file. It walks you through every choice and explains what each one means:

- **Pick a target.** Each option is described inline: **demo** is the local
  GPU-free benchmark backend (seeds demo `obench-*` models + tenants, no real
  keys, no cost); **live** is your real upstream API (real requests, real cost).
- **Live needs nothing pre-configured.** Choose live and the wizard asks for the
  upstream **base URL**, then the **API key** (input hidden). It then calls
  `GET {base}/models`, lists what the endpoint actually serves, and lets you
  multi-select which models to benchmark.
- **Demo single-model scope** shows a model picker — no `--model` flag needed.
- **Confirm screen.** Before anything is created, the wizard prints exactly what
  it will do: which models it registers in the gateway, which tenants and API
  keys it creates, and — for live — a cost warning. Nothing is seeded until you
  confirm.
- **Automatic cleanup.** Everything obench creates for a run (models it
  registered, the synthetic tenants, and the minted API keys) is deleted again
  the moment the run ends — on normal completion, a stall, or Ctrl-C. **API key
  secrets are held in memory only and are never written to disk.**
- **Errors never crash the TUI.** A bad URL, wrong key, or unreachable gateway
  shows a dismissible message and drops you back to the wizard to try again.

Connection defaults (`--admin-base`, `--proxy-base`, `--admin-token`,
`--ui-base`) match the local docker-compose stack and can also be set via
environment variables `ADMIN_BASE`, `ADMIN_TOKEN`, `PROXY_BASE`, `UI_BASE`.

## Security model

obench creates real gateway objects (models, tenants, API keys) through the
admin API, so it is built to leave nothing behind:

- **`demo` is local-only.** Because a demo run seeds synthetic models, tenants,
  and keys into the gateway it points at, `--target demo` is rejected unless
  `--admin-base` and `--proxy-base` resolve to this node (`localhost`,
  `127.0.0.1`, `::1`, `0.0.0.0`). To exercise a remote gateway, use
  `--target live` and give it that gateway's endpoint + key.
- **Keys never touch disk.** Minted API key secrets live only in memory for the
  duration of a run. There is no `keys.json` (or any other) key cache.
- **Automatic teardown.** When a run ends — success, stall, or Ctrl-C — obench
  deletes the API keys it minted, the tenants it created, and (for live) the
  model route it registered (which holds your upstream key). Objects that
  already existed and were merely updated are left intact; only what obench
  *created* is removed.

## Target × Profile × Scope

Every run is described by three dimensions:

| Dimension | Values | Meaning |
|-----------|--------|---------|
| **Target** | `demo`, `live` | Where to send requests. `demo` uses the GPU-free `benchmark-backend` container and is **local-only**. `live` uses real upstream APIs and may target a remote gateway. |
| **Profile** | `smoke`, `light`, `heavy`, `extreme`, `auto`, `manual` | Load intensity and duration. See below. |
| **Scope** | `--all` (default), `--model <name>` | Drive the full demo fleet or a single named model. |

**Validity constraint:** `--target live --profile extreme` is blocked. `extreme`
measures the gateway's raw req/s ceiling using tiny 4-token outputs against the
GPU-free demo backend, where generation time is negligible. Against live
upstreams the generation time dominates and the number is not meaningful. Use
`--target demo` for extreme, or pick `auto` or `heavy` for live.

### Profiles

| Profile | Concurrency | Duration | Output tokens | Stream | Purpose |
|---------|-------------|----------|---------------|--------|---------|
| `smoke` | 2 | 30 s | 16 | yes | Check the stack responds at all |
| `light` | 16 | 60 s | 64 | yes | Routine CI / sanity check |
| `heavy` | 64 | 600 s | 128 | yes | Sustained realistic load |
| `extreme` | 2048 | 30 s | 4 | no | Max req/s ceiling (demo only) |
| `auto` | ramp | auto | 4 | no | Self-calibrating (see below) |
| `manual` | 64 | 60 s | 64 | yes | Preset defaults, overridden by CLI flags |

Per-profile defaults can be overridden with `--conc`, `--duration-s`,
`--output-tokens`, `--input-tokens`, `--stream`, `--capacity`, and
`--max-error-rate`. `manual` starts from the defaults above (concurrency 64,
60 s, 64 output tokens, streaming) and exists specifically to be reshaped by
those flags.

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
obench --target demo --profile manual --conc 256 --output-tokens 4 --all
```

## The `obench-` demo set (idempotent seeding)

For `--target demo`, obench seeds the gateway before every run with a
canonical set of models, fairshare groups, and tenants whose names all start with
`obench-`. This prefix is the identity contract:

- **Models** (`obench-turbo`, `obench-base`, `obench-code`, `obench-large`,
  `obench-embed`) are registered against the demo backend with the correct
  upstream URL. If they already exist, obench updates them in place — it does
  not create duplicates.
- **Tenants** and **fairshare groups** follow the same upsert logic: if a
  tenant named `obench-chatbot` already exists, the run reuses it. The seeded
  fleet spans three groups so a run produces genuine cross-tenant contention:

  | Tenant | Group | Weight |
  |--------|-------|--------|
  | `obench-chatbot` | `obench-chatbot` | 500 |
  | `obench-chatbot-2` | `obench-chatbot` | 500 |
  | `obench-api-batch` | `obench-api` | 50 |
  | `obench-analytics` | `obench-analytics` | 100 |
  | `obench-embeddings` | `obench-api` | 50 |

- **API keys** are minted fresh for each run and held in memory only. Any stale
  same-named obench keys on a tenant are pruned first, so there is no test-key
  sprawl. When the run ends, the keys obench minted (and the tenants/models it
  created) are deleted automatically — nothing is written to disk and no
  credentials are left on the gateway.

Nothing generated is written into the source directory.

## Live config

`--target live` points obench at a **remote obleth gateway** you do not control.
obench acts as a pure black-box client: it never seeds models, never uses an
admin token, and never tears anything down. You supply the gateway URL, the model
names to drive, and one or more **real tenant API keys** you already hold.

The interactive TUI builds this for you (gateway URL → add keys → pick models),
so a config file is only needed for **headless** live runs. For `--target live`
in headless mode, obench reads a JSON config file (default `live.config.json`,
override with `--config <path>`):

```json
{
  "proxy_url": "https://gateway.example.com",
  "models": ["my-model-a", "my-model-b"],
  "keys": [
    { "label": "tenant-a", "weight": 100, "secret": "${OBENCH_KEY_A}" },
    { "label": "tenant-b", "weight": 200, "secret": "${OBENCH_KEY_B}" }
  ]
}
```

`proxy_url` is the OpenAI-compatible base of the remote gateway (with or without a
trailing `/v1`). Each entry in `keys[]` is a distinct **tenant** — add two or more
to drive genuine fairshare contention on the remote gateway. `weight` shapes how
much load each tenant generates; `label` is cosmetic (used in the dashboard and
saved config). The models you list must already exist on the remote gateway.

Any value may contain `${VAR}` placeholders. obench expands them from the
environment at load time. A missing variable is a **hard error** — it is never
silently replaced with an empty string. This prevents accidental runs with blank
API keys.

**Safety warning:** live runs send real requests to a real remote gateway using
real keys. Every completion may incur cost on that gateway. Key secrets are held
in memory only — they are never written to disk by obench (the saved
`.obench.json` keeps labels and weights, never secrets). Set `BENCH_OUT_DIR` to a
path outside the repo if the default `/tmp/obleth-bench` is not suitable for your
environment.

`--all` requires at least 1 model and 1 key in the config. To exercise fairshare,
supply 2+ keys so load spreads across distinct tenants.
`--model <name>` requires that the named model appear in `models[]`.

## Artifacts

All output goes to `BENCH_OUT_DIR` (default `/tmp/obleth-bench`). Files written
per run:

| File | Written by | Contents |
|------|-----------|---------|
| `<profile>-meta.json` | every profile | target, profile, scope, completions, req/s, error rate, p50/p99 TTFB, token counts, verdict |
| `<profile>-timeline.jsonl` | `--target demo` runs | per-10-second rows: `in_flight`, `queued` (sampled from the gateway's fairshare state, which is only observable on the local demo target) |
| `auto-meta.json` | `auto` profile | above + `sustainable_conc`, step history, `replay` block |

No secrets are ever written to `BENCH_OUT_DIR` — API keys live in memory only and
are deleted from the gateway during teardown.

After every run, obench prints a summary and two control-plane URLs:

```
verdict: PASS — deployment stayed up and served the load
requests: 4201 ok / 4215 attempts  (70 req/s)
errors: 0.33%   429: 0
ttfb ms: p50=42  p90=88  p99=140
tokens: in 512000 out 268864
watch in the control plane:
  fairshare   http://localhost:3002/fairshare
  accounting  http://localhost:3002/usage
```

Fairshare ratios, per-tenant accounting, and ledger reconciliation are things you
observe in those views — obench does not assert them automatically.
