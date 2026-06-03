import { mkdirSync } from "node:fs";
import { join, resolve } from "node:path";
import "./env.mjs";

export const BENCH_OUT_DIR = resolve(process.env.BENCH_OUT_DIR ?? "/tmp/obleth-bench");

mkdirSync(BENCH_OUT_DIR, { recursive: true });

export const benchPaths = {
  keys: join(BENCH_OUT_DIR, "keys.json"),
  runMeta: join(BENCH_OUT_DIR, "run-meta.json"),
  fairshareSamples: join(BENCH_OUT_DIR, "fairshare-samples.jsonl"),
};
