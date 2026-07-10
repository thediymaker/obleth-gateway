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
    for (const t of ["120", "30:00", "4:00:00", "0-04:00:00", "1-00", "2-12:30"]) {
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
    expect(GRES_RE.test("gpu")).toBe(false);
  });
  it("mem accepts size with optional unit", () => {
    expect(MEM_RE.test("560G")).toBe(true);
    expect(MEM_RE.test("4096")).toBe(true);
    expect(MEM_RE.test("32GB")).toBe(true);
    expect(MEM_RE.test("big")).toBe(false);
  });
});

describe("validateManagedModelForm", () => {
  it("returns no errors for a valid form", () => {
    expect(validateManagedModelForm(base())).toEqual({});
  });

  it("requires a partition", () => {
    expect(validateManagedModelForm(base({ slurm_partition: "  " }))).toHaveProperty("slurm_partition");
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
