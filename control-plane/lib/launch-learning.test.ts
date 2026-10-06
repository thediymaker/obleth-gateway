import { describe, expect, it } from "vitest";
import type { RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { baselineForm, setField } from "./deploy-form";
import { learnFrom, type LaunchRecord } from "./launch-learning";
import { EMPTY_CLUSTER } from "./recipe-inputs";

const now = Date.parse("2026-09-28T12:00:00Z");
const p = { apiModelName: "m", modelType: "chat", engine: "vllm", port: 8000, healthPath: "/health", targetReplicas: 1, maxJobFailures: 2, partition: "gh200", scriptBody: "", rawBody: "", warnings: [], kind: "model", inputs: [] } as RecipeDeployPreview;
const form = setField(baselineForm("r", p, EMPTY_CLUSTER), "slurm.time_limit", "1-00:00:00");
let n = 0;
const run = (over: Partial<LaunchRecord>): LaunchRecord => ({ id: String(n++), model_name: "m", recipe_id: "r", partition: "gh200", account: "ai", qos: "normal", time_limit: "1-00:00:00", nodes_requested: 1, nodes: "gh-1", submitted_at: new Date(now - 3_600_000).toISOString(), started_at: null, healthy_at: "x", ended_at: null, end_state: null, queued_secs: 180, load_secs: 4200, ...over });

describe("learning from past launches", () => {
  it("prefills what every recent launch agreed on", () => {
    const l = learnFrom([run({}), run({})], form, { now });
    expect(l.prefill).toEqual({ "slurm.account": "ai", "slurm.qos": "normal" });
    expect(l.prefillWhy["slurm.account"]).toBe("The last 2 launches of this recipe used ai.");
  });

  it("doesn't prefill when launches disagree", () => {
    expect(learnFrom([run({}), run({ account: "other" })], form, { now }).prefill).toEqual({ "slurm.qos": "normal" });
  });

  it("suggests the partition's longest walltime after runs hit the limit", () => {
    const l = learnFrom([run({ end_state: "TIMEOUT" }), run({ end_state: "TIMEOUT" }), run({ end_state: "CANCELLED" })], form, { now, partitionMaxMinutes: 10080 });
    const s = l.suggestions.find((x) => x.path === "slurm.time_limit")!;
    expect(s.value).toBe("7-00:00:00");
    expect(s.evidence).toBe("2 of 3 runs hit the 1 day walltime and were relaunched, reloading for ~70 min each time.");
  });

  it("suggests avoiding a node whose launches keep failing", () => {
    const l = learnFrom([run({ nodes: "gh-17", end_state: "NODE_FAIL" }), run({ nodes: "gh-17", end_state: "NODE_FAIL" }), run({ nodes: "gh-17", end_state: "COMPLETED" }), run({ nodes: "gh-2", end_state: "COMPLETED" })], form, { now });
    expect(l.suggestions.find((x) => x.path === "slurm.exclude")).toMatchObject({ value: "gh-17", title: "Avoid gh-17", evidence: "2 of 3 launches on gh-17 failed; on other nodes, 0 of 1." });
  });

  it("suggests more nodes after running out of memory", () => {
    const l = learnFrom([run({ end_state: "OUT_OF_MEMORY", healthy_at: null }), run({ nodes_requested: 4, end_state: "TIMEOUT" })], form, { now });
    expect(l.suggestions.find((x) => x.path === "slurm.nodes")).toMatchObject({ value: 4, title: "Run on 4 nodes" });
  });

  it("gives queue and load facts", () => {
    expect(learnFrom([run({}), run({ load_secs: 3600 })], form, { now }).facts.map((f) => f.label)).toEqual(["Queue wait on gh200", "Start to healthy", "Last launched"]);
  });
});
