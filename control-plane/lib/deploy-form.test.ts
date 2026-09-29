import { describe, expect, it } from "vitest";
import type { RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { baselineForm, changedPaths, formProblems, formToYaml, setField, toOverrides, yamlToForm } from "./deploy-form";
import { EMPTY_CLUSTER } from "./recipe-inputs";

const p: RecipeDeployPreview = {
  apiModelName: "nemotron-3-ultra", modelType: "chat", engine: "vllm", port: 8000, healthPath: "/health", targetReplicas: 1, maxJobFailures: 2, partition: "", nodes: 1,
  scriptBody: "", rawBody: "", warnings: [], kind: "model", nodeOptions: [1, 4, 8], env: { A: "1" },
  inputs: [
    { name: "offload_gb", type: "number", default: "270", by_nodes: { "4": "10" }, required: false, min: 0, max: 460 },
    { name: "mtp", type: "flag", default: "true", required: false },
  ],
};
const cv = { ...EMPTY_CLUSTER, logs: "/scratch/logs" };
const resources = { partitions: [{ name: "gh200", nodes: ["gh-1"], default_time: null, max_time: "10080" }], nodes: [{ name: "gh-1", partitions: ["gh200"], gres: "gpu:1", cpus: 72, real_memory_mb: 491520, features: [] }], accounts: ["ai-research"], qos: ["normal", "long"] };

describe("the deploy form", () => {
  const base = baselineForm("nemotron", p, cv);

  it("starts from the recipe, with cluster log folder and inputs for the node count", () => {
    expect(base.slurm.log_output_dir).toBe("/scratch/logs");
    expect(base.inputs).toEqual({ offload_gb: "270", mtp: "true" });
    expect(baselineForm("nemotron", { ...p, nodes: 4 }, cv).inputs.offload_gb).toBe("10");
  });

  it("knows what changed and sends numbers as numbers", () => {
    const f = setField(setField(base, "slurm.qos", "long"), "slurm.nodes", 4);
    expect(changedPaths(f, base)).toEqual(["slurm.qos", "slurm.nodes"]);
    expect(toOverrides(f)).toMatchObject({ qos: "long", nodes: 4, target_replicas: 1, min_replicas: 1, inputs: { offload_gb: "270", mtp: "true" }, env: { A: "1" } });
  });

  it("checks against the cluster and suggests near misses", () => {
    const f = setField(setField(setField(base, "slurm.partition", "gh200"), "slurm.qos", "lng"), "slurm.time_limit", "8-00:00:00");
    const msgs = formProblems(f, p, resources, []).map((x) => x.message);
    expect(msgs).toContain("lng isn't a QoS your user has. Did you mean long?");
    expect(msgs.some((m) => m.includes("over gh200's 7 days limit"))).toBe(true);
  });

  it("round-trips through YAML, writing only what changed", () => {
    const f = setField(setField(base, "slurm.account", "ai-research"), "inputs.mtp", "false");
    const text = formToYaml(f, base, { notes: { "slurm.account": "learned" } });
    expect(text).toContain("account: ai-research # learned");
    expect(text).toContain("mtp: false");
    expect(text).not.toContain("offload_gb");
    const back = yamlToForm(text, base, ["offload_gb", "mtp"]);
    expect(back.problems).toEqual([]);
    expect(back.form).toEqual(f);
  });

  it("points at the line of an unknown key or bad value", () => {
    const res = yamlToForm("recipe: nemotron\nslurm:\n  nodes: many\ninputs:\n  context: 1\n", base, ["offload_gb", "mtp"]);
    expect(res.form).toBeNull();
    expect(res.problems).toEqual([{ line: 3, message: "nodes should be a number" }, { line: 5, message: "context isn't one of this recipe's inputs (offload_gb, mtp)" }]);
  });

  it("removes an environment variable given an empty value", () => {
    expect(yamlToForm("env:\n  A:\n", base, []).form?.env).toEqual({});
  });
});
