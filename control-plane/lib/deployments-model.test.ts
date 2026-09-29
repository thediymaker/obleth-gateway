import { describe, expect, it } from "vitest";
import {
  accountsFor,
  slurmErrorText,
  buildDeploymentRows,
  countReplicas,
  deploymentsLine,
  formatTimeLimit,
  gresCount,
  managedState,
  memToMb,
  nodeState,
  parseTimeLimit,
  partitionFits,
  preflightChecks,
  replicaPhase,
  slurmStatus,
  walltimeChoices,
  walltimeLabel,
  watchedState,
} from "./deployments-model";
import type { CapacityDiscoveryModelView, ClusterResources, ManagedModelSpec, ModelReplica, ModelRoute } from "./obleth";

const replica = (over: Partial<ModelReplica> = {}): ModelReplica => ({
  id: Math.random().toString(36).slice(2), model_id: "m1", slurm_job_id: "4817203", nodes: "gh-007", endpoint_id: null,
  state: "pending", last_message: null, created_at: "2026-09-28T10:00:00Z", updated_at: "2026-09-28T10:05:00Z", ...over,
});

const spec = (over: Partial<ManagedModelSpec> = {}): ManagedModelSpec => ({
  model_id: "m1", enabled: true, partition: "gh200", gres: "gpu:1", nodes: 1, constraints: null, exclude: null, account: "ai-gateway",
  qos: "normal", time_limit: "1-00:00:00", cpus_per_task: 72, mem: "560G", image: "", preamble: "", log_output_dir: "", launch_command: "",
  script_body: "llama-server", serving_port: 8000, health_path: "/health", min_replicas: 1, target_replicas: 2, max_job_failures: 3,
  launcher_spec: { source: "recipe", engine: "llama.cpp", name: "Deploy llama.cpp" }, created_at: "", updated_at: "", ...over,
});

describe("replicas", () => {
  it("reads Slurm's state and reason from a replica's message", () => {
    expect(slurmStatus("PENDING — Resources")).toEqual({ state: "PENDING", reason: "Resources" });
    expect(slurmStatus("RUNNING")).toEqual({ state: "RUNNING", reason: null });
    expect(slurmStatus(null)).toEqual({ state: "", reason: null });
  });

  it("tells a queued job from one that is running but not yet healthy", () => {
    expect(replicaPhase(replica({ last_message: "PENDING — Priority" }))).toBe("queued");
    expect(replicaPhase(replica({ last_message: "RUNNING" }))).toBe("running");
    expect(replicaPhase(replica({ state: "healthy" }))).toBe("serving");
    expect(countReplicas([replica({ state: "healthy" }), replica({ last_message: "RUNNING" }), replica({ state: "lost" })])).toEqual({ healthy: 1, queued: 0, running: 1, draining: 0, lost: 1 });
  });
});

describe("a Slurm deployment's state", () => {
  it("is serving at its target and starting below it, with Slurm's reason", () => {
    expect(managedState(spec(), [replica({ state: "healthy" }), replica({ state: "healthy" })]).state).toBe("serving");
    const v = managedState(spec(), [replica({ state: "healthy" }), replica({ last_message: "PENDING — Resources" })]);
    expect(v).toEqual({ state: "starting", why: "1 queued: Resources", attention: false });
  });

  it("is stopped once failed launches reach the limit, even with a replica serving", () => {
    const lost = [1, 2, 3].map(() => replica({ state: "lost", last_message: "job gone" }));
    expect(managedState(spec(), lost)).toMatchObject({ state: "stopped", attention: true, why: "3 failed launches" });
    expect(managedState(spec(), [...lost, replica({ state: "healthy" })]).why).toBe("3 failed launches · 1 still serving");
    expect(managedState(spec({ max_job_failures: 0 }), lost).state).toBe("not-running");
  });

  it("is paused when turned off", () => {
    expect(managedState(spec({ enabled: false }), [replica({ state: "draining" })])).toMatchObject({ state: "paused", why: "stopping its jobs" });
  });
});

describe("a watched Kubernetes deployment's state", () => {
  const disc = (ready: number, state = "discovered") => ({ status: { ready_replicas: ready, state, reason: null } }) as unknown as CapacityDiscoveryModelView;
  it("needs you when the Service has no ready replicas", () => {
    expect(watchedState(disc(10), { enabled: true }).state).toBe("serving");
    expect(watchedState(disc(0), { enabled: true })).toMatchObject({ state: "no-replicas", attention: true });
    expect(watchedState(disc(4, "stale"), { enabled: true }).state).toBe("stale");
  });
});

describe("rows", () => {
  it("includes launched and watched models, not plain endpoints", () => {
    const models = [
      { id: "m1", model_name: "glm-5.2", capacity_source: "endpoints", max_in_flight: 8, enabled: true },
      { id: "m2", model_name: "glm-5-3-flash", capacity_source: "kubernetes", max_in_flight: null, enabled: true },
      { id: "m3", model_name: "openai-proxy", capacity_source: "endpoints", max_in_flight: 16, enabled: true },
    ] as unknown as ModelRoute[];
    const discovery = [{ model_id: "m2", enforced_max_in_flight: 160, in_flight: 38, cluster_in_flight: 38, status: { namespace: "aibrix-system-llm", service: "glm-5-3-flash", ready_replicas: 10, per_replica_max_in_flight: 16, effective_max_in_flight: 160, state: "discovered" } }] as unknown as CapacityDiscoveryModelView[];
    const rows = buildDeploymentRows(models, [spec()], [replica({ state: "healthy" }), replica({ last_message: "PENDING — Resources" })], discovery, { "glm-5-3-flash": 16039 });
    expect(rows.map((r) => [r.model.model_name, r.kind, r.ready, r.wanted, r.where])).toEqual([
      ["glm-5.2", "slurm", 1, 2, "gh200 · gpu:1 · 72 CPU · 560G · 1 day"],
      ["glm-5-3-flash", "kubernetes", 10, null, "aibrix-system-llm / glm-5-3-flash"],
    ]);
    expect(rows[1].poolCap).toBe(160);
    expect(deploymentsLine(rows)).toBe("1 on Kubernetes, 10 replicas ready · 1 on Slurm, 1 starting");
  });
});

describe("Slurm arithmetic", () => {
  it("counts GPUs and reads memory", () => {
    expect(gresCount("gpu:1")).toBe(1);
    expect(gresCount("gpu:h100:2")).toBe(2);
    expect(gresCount("")).toBe(0);
    expect(memToMb("560G")).toBe(573440);
    expect(memToMb("4096M")).toBe(4096);
    expect(memToMb("lots")).toBeNull();
  });

  it("reads and writes walltime", () => {
    expect(parseTimeLimit("1-00:00:00")).toBe(1440);
    expect(parseTimeLimit("4:00:00")).toBe(240);
    expect(parseTimeLimit("90")).toBe(90);
    expect(parseTimeLimit("2-12")).toBe(3600);
    expect(formatTimeLimit(1440)).toBe("1-00:00:00");
    expect(walltimeLabel(3600)).toBe("2 days 12 hours");
    expect(walltimeChoices(2880)).toEqual([240, 720, 1440, 2880]);
    expect(walltimeChoices(1000)).toEqual([240, 720, 1000]);
  });

  it("reads node state flags", () => {
    expect(nodeState(["IDLE"])).toBe("idle");
    expect(nodeState(["MIXED"])).toBe("busy");
    expect(nodeState(["IDLE", "DRAIN"])).toBe("down");
    expect(nodeState(undefined)).toBe("idle");
  });

  const resources: ClusterResources = {
    partitions: [
      { name: "gh200", nodes: [], default_time: null, max_time: "2880" },
      { name: "a100", nodes: [], default_time: null, max_time: "1440" },
      { name: "general", nodes: [], default_time: null, max_time: null },
    ],
    nodes: [
      { name: "gh-001", partitions: ["gh200"], gres: "gpu:gh200:1", cpus: 72, real_memory_mb: 587776, features: [], state: ["IDLE"] },
      { name: "gh-002", partitions: ["gh200"], gres: "gpu:gh200:1", cpus: 72, real_memory_mb: 587776, features: [], state: ["ALLOCATED"] },
      { name: "a-001", partitions: ["a100"], gres: "gpu:a100:4(S:0-1)", cpus: 64, real_memory_mb: 524288, features: [], state: ["IDLE"] },
      { name: "c-001", partitions: ["general"], gres: "", cpus: 128, real_memory_mb: 524288, features: [], state: ["IDLE"] },
    ],
    accounts: ["ai-gateway"],
    qos: ["normal", "high"],
  };

  it("says which partitions a replica fits and why not", () => {
    const fits = partitionFits(resources, { gpus: 1, cpus: 72, memMb: memToMb("560G") });
    expect(fits.map((f) => [f.name, f.fits, f.reason, f.idleFitting])).toEqual([
      ["gh200", true, null, 1],
      ["a100", false, "needs 560G, nodes have 512G", 0],
      ["general", false, "no GPUs", 0],
    ]);
    expect(fits[0].maxMinutes).toBe(2880);
    expect(fits[1].shape).toBe("4 GPU · 64 CPU · 512G per node");
  });

  it("checks what it can before submitting", () => {
    const [gh] = partitionFits(resources, { gpus: 1, cpus: 72, memMb: memToMb("560G") });
    const checks = preflightChecks({ fit: gh, minutes: 4320, nameFree: false, name: "glm-5.2", account: "other", accounts: resources.accounts, qos: "normal", qosList: resources.qos });
    expect(checks.filter((c) => !c.ok).map((c) => c.text)).toEqual([
      "3 days is over gh200's 2 days limit",
      "Your Slurm user has no association with other",
      "A model called glm-5.2 already exists",
    ]);
  });
});

describe("accounts a partition takes", () => {
  const base = { nodes: [], accounts: ["grp_rcadmins", "grp_ml"], qos: [] };
  it("uses associations for the partition and the partition's own rules", () => {
    const r = { ...base, partitions: [{ name: "arm", nodes: [], default_time: null, max_time: null, allowed_accounts: ["grp_ml"] }, { name: "gh200", nodes: [], default_time: null, max_time: null }], associations: [{ account: "grp_rcadmins", partition: null, qos: [] }, { account: "grp_ml", partition: "arm", qos: [] }] };
    expect(accountsFor(r, "arm")).toEqual(["grp_ml"]);
    expect(accountsFor(r, "gh200")).toEqual(["grp_rcadmins"]);
  });
  it("can't tell without associations or partition rules", () => {
    expect(accountsFor({ ...base, partitions: [{ name: "arm", nodes: [], default_time: null, max_time: null }] }, "arm")).toBeNull();
  });
  it("pulls Slurm's own words out of a slurmrestd error", () => {
    const raw = 'slurmrestd submit failed (500 Internal Server Error): {"errors":[{"description":"Batch job submission failed","error":"Invalid account or account/partition combination specified","error_number":2045}],"warnings":[{"description":"Expected OpenAPI type=array"}]}';
    expect(slurmErrorText(raw)).toBe("Invalid account or account/partition combination specified");
    expect(slurmErrorText("connection refused")).toBe("connection refused");
  });
});
