#!/usr/bin/env python3
"""
Concurrency-sweep load test for the kompress /score endpoint.

Stdlib only — no external deps, runs anywhere Python 3.9+ is available. Drives a
RUNNING sidecar at increasing concurrency and reports p50/p95/p99 latency,
throughput (requests/s and sentences/s), and error rate, so you can find the
per-pod capacity knee and size the HPA target.

Typical use:

  # start a sidecar (see kompress/bench/README.md), then:
  python loadtest.py --url http://127.0.0.1:8899 \
      --concurrency 1,2,4,8,16,32 --requests 200 --sentences 40

Reading the output: throughput climbs with concurrency until the pod saturates,
then flattens while p95/p99 shoot up. That inflection is the per-pod capacity.
Set the HPA to scale a bit below the concurrency where p95 crosses your SLO.
"""
from __future__ import annotations

import argparse
import json
import statistics
import subprocess
import threading
import time
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed


def make_payload(n_sentences: int, words_per_sentence: int) -> bytes:
    """A deterministic prose-ish payload of `n_sentences`, each ~`words_per_sentence` words."""
    filler = " ".join(["token"] * max(1, words_per_sentence))
    sents = [f"Component number {i} reports that {filler} completed successfully." for i in range(n_sentences)]
    return json.dumps({"segments": [{"sentences": sents}]}).encode("utf-8")


def one_request(url: str, body: bytes, timeout: float) -> tuple[float, bool]:
    req = urllib.request.Request(
        url + "/score", data=body, headers={"Content-Type": "application/json"}
    )
    t0 = time.perf_counter()
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            resp.read()
            ok = resp.status == 200
    except Exception:
        ok = False
    return time.perf_counter() - t0, ok


def percentile(sorted_vals: list[float], p: float) -> float:
    if not sorted_vals:
        return float("nan")
    k = (len(sorted_vals) - 1) * p
    lo = int(k)
    hi = min(lo + 1, len(sorted_vals) - 1)
    if lo == hi:
        return sorted_vals[lo]
    return sorted_vals[lo] + (sorted_vals[hi] - sorted_vals[lo]) * (k - lo)


class CpuSampler:
    """Best-effort `docker stats` CPU% sampler for a named container."""

    def __init__(self, container: str | None):
        self.container = container
        self.samples: list[float] = []
        self._stop = threading.Event()
        self._thread: threading.Thread | None = None

    def _loop(self) -> None:
        while not self._stop.is_set():
            try:
                out = subprocess.run(
                    ["docker", "stats", "--no-stream", "--format", "{{.CPUPerc}}", self.container],
                    capture_output=True, text=True, timeout=5,
                )
                val = out.stdout.strip().rstrip("%")
                if val:
                    self.samples.append(float(val))
            except Exception:
                pass
            self._stop.wait(0.5)

    def __enter__(self) -> "CpuSampler":
        if self.container:
            self._thread = threading.Thread(target=self._loop, daemon=True)
            self._thread.start()
        return self

    def __exit__(self, *exc: object) -> None:
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=2)

    def summary(self) -> str:
        if not self.samples:
            return "-"
        return f"{statistics.mean(self.samples):.0f}/{max(self.samples):.0f}"


def run_level(url: str, body: bytes, concurrency: int, total: int, timeout: float, container: str | None) -> dict:
    latencies: list[float] = []
    errors = 0
    with CpuSampler(container) as cpu:
        t0 = time.perf_counter()
        with ThreadPoolExecutor(max_workers=concurrency) as ex:
            futures = [ex.submit(one_request, url, body, timeout) for _ in range(total)]
            for fut in as_completed(futures):
                dur, ok = fut.result()
                latencies.append(dur)
                if not ok:
                    errors += 1
        wall = time.perf_counter() - t0
        cpu_summary = cpu.summary()
    latencies.sort()
    rps = total / wall if wall > 0 else 0.0
    return {
        "concurrency": concurrency,
        "rps": rps,
        "p50_ms": percentile(latencies, 0.50) * 1000,
        "p95_ms": percentile(latencies, 0.95) * 1000,
        "p99_ms": percentile(latencies, 0.99) * 1000,
        "errors": errors,
        "cpu": cpu_summary,
    }


def main() -> None:
    ap = argparse.ArgumentParser(description="kompress /score concurrency sweep")
    ap.add_argument("--url", default="http://127.0.0.1:8899", help="sidecar base URL")
    ap.add_argument("--concurrency", default="1,2,4,8,16,32", help="comma-separated concurrency levels")
    ap.add_argument("--requests", type=int, default=200, help="requests per concurrency level")
    ap.add_argument("--sentences", type=int, default=40, help="sentences per request")
    ap.add_argument("--words", type=int, default=16, help="words per sentence")
    ap.add_argument("--timeout", type=float, default=30.0, help="per-request timeout (s)")
    ap.add_argument("--warmup", type=int, default=10, help="warmup requests before measuring")
    ap.add_argument("--docker-name", default=None, help="container name to sample CPU%% via docker stats")
    args = ap.parse_args()

    levels = [int(c) for c in args.concurrency.split(",") if c.strip()]
    body = make_payload(args.sentences, args.words)

    # Health + warmup.
    try:
        with urllib.request.urlopen(args.url + "/health", timeout=5) as resp:
            health = json.loads(resp.read())
        print(f"sidecar: {health.get('model')} rev {str(health.get('revision'))[:7]} @ {args.url}")
    except Exception as exc:
        raise SystemExit(f"sidecar not reachable at {args.url}/health: {exc}")
    for _ in range(args.warmup):
        one_request(args.url, body, args.timeout)

    print(
        f"payload: {args.sentences} sentences x ~{args.words} words | "
        f"{args.requests} req/level | CPU%% avg/max shown when --docker-name set\n"
    )
    header = f"{'conc':>4} {'req/s':>8} {'sent/s':>9} {'p50 ms':>9} {'p95 ms':>9} {'p99 ms':>9} {'err':>4} {'cpu a/m':>9}"
    print(header)
    print("-" * len(header))
    for c in levels:
        r = run_level(args.url, body, c, args.requests, args.timeout, args.docker_name)
        print(
            f"{r['concurrency']:>4} {r['rps']:>8.1f} {r['rps']*args.sentences:>9.0f} "
            f"{r['p50_ms']:>9.1f} {r['p95_ms']:>9.1f} {r['p99_ms']:>9.1f} "
            f"{r['errors']:>4} {r['cpu']:>9}"
        )


if __name__ == "__main__":
    main()
