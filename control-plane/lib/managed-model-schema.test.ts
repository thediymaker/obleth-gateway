import { describe, expect, it } from "vitest";
import {
  GRES_RE,
  MEM_RE,
  SLURM_TIME_LIMIT_RE,
  validateManagedModelForm,
} from "./managed-model-schema";

/** A fully-valid baseline; individual tests override single fields. */
function base(overrides: Record<string, string> = {}): Record<string, string> {
  return {
    slurm_partition: "arm",
    slurm_gres: "gpu:1",
    slurm_cpus_per_task: "72",
    slurm_mem: "560G",
    slurm_nodes: "1",
    slurm_qos: "private",
    slurm_account: "",
    slurm_time_limit: "0-04:00:00",
    slurm_constraints: "",
    slurm_exclude: "",
    slurm_serving_port: "8000",
    slurm_health_path: "/health",
    slurm_min_replicas: "1",
    slurm_target_replicas: "2",
    slurm_max_job_failures: "3",
    ...overrides,
  };
}

describe("SLURM_TIME_LIMIT_RE", () => {
  it("accepts valid Slurm walltime formats", () => {
    for (const t of ["120", "30:00", "4:00:00", "0-04:00:00", "1-00", "2-12:30", "UNLIMITED", "infinite"]) {
      expect(SLURM_TIME_LIMIT_RE.test(t)).toBe(true);
    }
  });
  it("rejects malformed walltimes", () => {
    for (const t of ["4h", "1:2:3:4", "04:00:00:00", "abc", "1-"]) {
      expect(SLURM_TIME_LIMIT_RE.test(t)).toBe(false);
    }
  });
});

describe("GRES_RE / MEM_RE", () => {
  it("gres accepts name:count and name:type:count", () => {
    expect(GRES_RE.test("gpu:1")).toBe(true);
    expect(GRES_RE.test("gpu:h100:2")).toBe(true);
    for (const g of ["gpu", "gpu:nvidia_a100-sxm4-80gb:1", "gpu:3g.40gb:1", "gpu:1,shard:1", "gpu:h100"]) expect(GRES_RE.test(g)).toBe(true);
    for (const g of ["gpu:", "gpu:1,", ":1", "gpu 1"]) expect(GRES_RE.test(g)).toBe(false);
  });
  it("mem accepts size with optional unit", () => {
    expect(MEM_RE.test("560G")).toBe(true);
    expect(MEM_RE.test("4096")).toBe(true);
    expect(MEM_RE.test("32GB")).toBe(true);
    expect(MEM_RE.test("big")).toBe(false);
  });
});

describe("validateManagedModelForm", () => {
  it("takes Job logs as a full path or blank", () => {
    expect(validateManagedModelForm(base({ slurm_log_output_dir: "/scratch/me/logs" }))).toEqual({});
    expect(validateManagedModelForm(base({ slurm_log_output_dir: "" }))).toEqual({});
    expect(validateManagedModelForm(base({ slurm_log_output_dir: "logs" })).slurm_log_output_dir).toContain("full path");
  });

  it("returns no errors for a valid form", () => {
    expect(validateManagedModelForm(base())).toEqual({});
  });

  it("takes a blank partition as the cluster's default", () => {
    expect(validateManagedModelForm(base({ slurm_partition: "  " }))).toEqual({});
  });

  it("checks only what changed, so a stored value an older check let through doesn't block other edits", async () => {
    const { specFormValues } = await import("./managed-model-schema");
    const stored = base({ slurm_gres: "gpu:odd type:1", slurm_min_replicas: "0" });
    expect(validateManagedModelForm({ ...stored, slurm_max_job_failures: "5" }, stored)).toEqual({});
    expect(validateManagedModelForm({ ...stored, slurm_gres: "gpu:bad type:2" }, stored)).toHaveProperty("slurm_gres");
    expect(specFormValues({ partition: "p", gres: "gpu:1", nodes: 1, cpus_per_task: null, mem: null, account: null, qos: null, time_limit: null, constraints: null, exclude: null, serving_port: 8000, health_path: "/health", min_replicas: 1, target_replicas: 2, max_job_failures: 3 }).slurm_cpus_per_task).toBe("");
  });

  it("checks the startup timeout and handoff as whole minutes", () => {
    expect(validateManagedModelForm(base({ slurm_startup_timeout_mins: "0", slurm_handoff_mins: "15" }))).toEqual({});
    expect(validateManagedModelForm(base({ slurm_handoff_mins: "-1" }))).toHaveProperty("slurm_handoff_mins");
  });

  it("flags a bad time limit", () => {
    expect(validateManagedModelForm(base({ slurm_time_limit: "4h" }))).toHaveProperty("slurm_time_limit");
  });

  it("allows blank optional fields", () => {
    const errors = validateManagedModelForm(
      base({ slurm_gres: "", slurm_mem: "", slurm_cpus_per_task: "", slurm_time_limit: "" }),
    );
    expect(errors).toEqual({});
  });

  it("rejects an out-of-range port", () => {
    expect(validateManagedModelForm(base({ slurm_serving_port: "70000" }))).toHaveProperty("slurm_serving_port");
    expect(validateManagedModelForm(base({ slurm_serving_port: "0" }))).toHaveProperty("slurm_serving_port");
    expect(validateManagedModelForm(base({ slurm_serving_port: "65533" }))).toHaveProperty("slurm_serving_port");
  });

  it("requires a health path to start with /", () => {
    expect(validateManagedModelForm(base({ slurm_health_path: "health" }))).toHaveProperty("slurm_health_path");
  });

  it("rejects min replicas greater than target", () => {
    expect(
      validateManagedModelForm(base({ slurm_min_replicas: "5", slurm_target_replicas: "2" })),
    ).toHaveProperty("slurm_min_replicas");
  });
});
