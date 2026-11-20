import { describe, expect, it } from "vitest";
import type { RecipeDeployPreview } from "@/components/recipes/recipe-card";
import { baselineForm, changedPaths, formProblems, formToYaml, setField, toOverrides, yamlToForm } from "./deploy-form";
import { EMPTY_CLUSTER } from "./recipe-inputs";
import { validateManagedModelForm } from "./managed-model-schema";
import { parseTimeLimit, walltimeLabel } from "./deployments-model";

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
    expect(msgs).toContain("There's no QoS called lng on this cluster. Did you mean long?");
    expect(msgs.some((m) => m.includes("over gh200's 7 days limit"))).toBe(true);
  });

  it("says which accounts a partition takes before Slurm refuses the job", () => {
    const r = { ...resources, partitions: [{ ...resources.partitions[0], allowed_accounts: ["ai-research"] }], accounts: ["ai-research", "grp_rcadmins"] };
    const f = setField(setField(base, "slurm.partition", "gh200"), "slurm.account", "grp_rcadmins");
    expect(formProblems(f, p, r, []).map((x) => x.message)).toContain("gh200 doesn't take grp_rcadmins. It takes ai-research.");
  });

  it("allows a blank partition, rejects a relative log folder and bad gres, and names a missing cluster default", () => {
    const withImage = { ...p, inputs: [{ name: "image", label: "Container image", type: "path" as const, default: "{{cluster.image.vllm}}", required: true }] };
    const f = setField(setField(setField(baselineForm("x", withImage, EMPTY_CLUSTER), "slurm.log_output_dir", "logs"), "slurm.gres", "gpu:"), "inputs.image", "{{cluster.image.vllm}}");
    const msgs = formProblems(f, withImage, resources, []).map((x) => x.message);
    expect(msgs.some((m) => m.includes("partition"))).toBe(false);
    expect(msgs.some((m) => m.startsWith("Job logs must be a full path"))).toBe(true);
    expect(msgs.some((m) => m.startsWith("Write GPUs as Slurm gres"))).toBe(true);
    expect(msgs).toContain("Container image comes from Cluster defaults › Image per engine › vLLM, which isn't set. Set it on the Slurm connection, or type a value here for this launch");
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

describe("launch names and model type", () => {
  it("refuses a name the engine or shell would split, and carries a model type from the lookup", async () => {
    const { baselineForm, formProblems, toOverrides } = await import("./deploy-form");
    const { EMPTY_CLUSTER } = await import("./recipe-inputs");
    const preview = { apiModelName: "m", modelType: "chat", engine: "vllm", port: 8000, healthPath: "/health", targetReplicas: 1, maxJobFailures: 2, partition: "", scriptBody: "", rawBody: "", warnings: [], inputs: [], kind: "engine" as const };
    const resources = { partitions: [], accounts: [], qos: [] } as unknown as Parameters<typeof formProblems>[2];
    const f = baselineForm("vllm", preview, EMPTY_CLUSTER, { name: "Gemma 4", modelType: "embedding" });
    expect(formProblems(f, preview, resources, []).map((p) => p.path)).toContain("name");
    expect(toOverrides(f).model_type).toBe("embedding");
    expect(baselineForm("vllm", { ...preview, maxJobFailures: undefined as unknown as number }, EMPTY_CLUSTER).serving.stop_after_failed_launches).toBe(2);
  });
});

describe("placement rules", () => {
  const base = baselineForm("nemotron", p, cv);
  // The deployment page's form, with this one placement field set.
  const page = (field: string, value: string) => validateManagedModelForm({
    slurm_partition: "", slurm_gres: "", slurm_cpus_per_task: "", slurm_mem: "", slurm_nodes: "1", slurm_qos: "", slurm_account: "",
    slurm_time_limit: "", slurm_constraints: "", slurm_exclude: "", slurm_serving_port: "8000", slurm_health_path: "/health",
    slurm_min_replicas: "1", slurm_target_replicas: "1", slurm_max_job_failures: "2", [`slurm_${field}`]: value,
  })[`slurm_${field}`];
  const wizard = (field: string, value: string) => formProblems(setField(base, `slurm.${field}`, value), p, { partitions: [], accounts: [], qos: [] } as unknown as typeof resources, []).find((x) => x.path === `slurm.${field}`)?.message;

  const cases: [string, string, boolean][] = [
    ["time_limit", "1-00:00:00", true], ["time_limit", "4:00:00", true], ["time_limit", "UNLIMITED", true], ["time_limit", "a day", false],
    ["mem", "64G", true], ["mem", "0", true], ["mem", "64 GB", false],
    ["cpus_per_task", "8", true], ["cpus_per_task", "0.5", false],
    ["gres", "gpu:h100:2", true], ["gres", "gpu:", false],
    ["partition", "", true],
  ];
  for (const [field, value, ok] of cases) {
    it(`${field} = ${JSON.stringify(value)} is ${ok ? "taken" : "refused"} the same way by the wizard and the page`, () => {
      expect(wizard(field, value)).toBe(page(field, value));
      expect(page(field, value) === undefined).toBe(ok);
    });
  }

  it("reads UNLIMITED as no limit", () => {
    expect(parseTimeLimit("UNLIMITED")).toBe(Infinity);
    expect(parseTimeLimit("infinite")).toBe(Infinity);
    expect(walltimeLabel(parseTimeLimit("UNLIMITED"))).toBe("no limit");
  });
});
