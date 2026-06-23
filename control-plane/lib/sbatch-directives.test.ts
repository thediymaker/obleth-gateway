import { describe, it, expect } from "vitest";
import { parseSbatchDirectives } from "./sbatch-directives";

describe("parseSbatchDirectives", () => {
  it("handles --key=value, --key value, and -k value forms", () => {
    const d = parseSbatchDirectives(
      [
        "#!/bin/bash -l",
        "#SBATCH --gres=gpu:gh200:1",
        "#SBATCH --time 04:00:00",
        "#SBATCH -p arm",
        "module load cuda",
      ].join("\n"),
    );
    expect(d.gres).toBe("gpu:gh200:1");
    expect(d.time_limit).toBe("04:00:00");
    expect(d.partition).toBe("arm");
  });

  it("strips trailing # comments from a directive value", () => {
    const d = parseSbatchDirectives("#SBATCH --gres=gpu:1   # one GH200");
    expect(d.gres).toBe("gpu:1");
  });

  it("maps every recognized directive to its field", () => {
    const d = parseSbatchDirectives(
      [
        "#SBATCH --partition=arm",
        "#SBATCH --cpus-per-task=72",
        "#SBATCH --mem=560G",
        "#SBATCH --nodes=2",
        "#SBATCH --account=proj1",
        "#SBATCH --qos=high",
        "#SBATCH --constraint=a100",
        "#SBATCH --exclude=node5",
      ].join("\n"),
    );
    expect(d).toMatchObject({
      partition: "arm",
      cpus_per_task: 72,
      mem: "560G",
      nodes: 2,
      account: "proj1",
      qos: "high",
      constraints: "a100",
      exclude: "node5",
    });
  });

  it("maps short forms -c -N -A -q -C -x -t", () => {
    const d = parseSbatchDirectives(
      [
        "#SBATCH -c 36",
        "#SBATCH -N 1",
        "#SBATCH -A acct",
        "#SBATCH -q normal",
        "#SBATCH -C zen4",
        "#SBATCH -x bad-node",
        "#SBATCH -t 01:00:00",
      ].join("\n"),
    );
    expect(d).toMatchObject({
      cpus_per_task: 36,
      nodes: 1,
      account: "acct",
      qos: "normal",
      constraints: "zen4",
      exclude: "bad-node",
      time_limit: "01:00:00",
    });
  });

  it("extracts the directory from --output and --error", () => {
    const d = parseSbatchDirectives("#SBATCH --output=logs/serve-%j.out");
    expect(d.log_output_dir).toBe("logs");
  });

  it("records --chdir for the deploy builder", () => {
    const d = parseSbatchDirectives("#SBATCH --chdir=/scratch/run");
    expect(d.chdir).toBe("/scratch/run");
  });

  it("collects unknown directives as warnings instead of dropping silently", () => {
    const d = parseSbatchDirectives(
      ["#SBATCH --array=0-3", "#SBATCH --nodelist=node7"].join("\n"),
    );
    expect(d.partition).toBeUndefined();
    expect(d.warnings.join(" ")).toContain("--array");
    expect(d.warnings.join(" ")).toContain("--nodelist");
  });

  it("parses the real glm52mu.sbatch directive block", () => {
    const d = parseSbatchDirectives(
      [
        "#!/bin/bash -l",
        "#SBATCH --job-name=glm-mu",
        "#SBATCH --gres=gpu:1                 # one GH200",
        "#SBATCH --cpus-per-task=72           # all Grace CPU cores",
        "#SBATCH --mem=560G                   # request host RAM explicitly",
        "#SBATCH --time=04:00:00",
        "#SBATCH -p arm",
        "#SBATCH --output=logs/serve-%j.out",
        "#SBATCH --error=logs/serve-%j.out",
        "set -euo pipefail",
      ].join("\n"),
    );
    expect(d).toMatchObject({
      gres: "gpu:1",
      cpus_per_task: 72,
      mem: "560G",
      time_limit: "04:00:00",
      partition: "arm",
      log_output_dir: "logs",
    });
    // --job-name is intentionally unmapped (obleth derives the job name).
    expect(d.warnings.join(" ")).toContain("--job-name");
  });

  it("unknown short flags produce clean warnings without triple-dash", () => {
    const d = parseSbatchDirectives("#SBATCH -J myjob");
    expect(d.warnings.join(" ")).toContain("-J");
    expect(d.warnings.join(" ")).not.toContain("---J");
  });

  it("empty numeric directives do not set the field to 0", () => {
    const d1 = parseSbatchDirectives("#SBATCH --cpus-per-task");
    expect(d1.cpus_per_task).toBeUndefined();
    const d2 = parseSbatchDirectives("#SBATCH --nodes");
    expect(d2.nodes).toBeUndefined();
  });

  it("maps short forms -o and -e to log_output_dir", () => {
    const d1 = parseSbatchDirectives("#SBATCH -o logs/run-%j.out");
    expect(d1.log_output_dir).toBe("logs");
    const d2 = parseSbatchDirectives("#SBATCH -e errors/e.out");
    expect(d2.log_output_dir).toBe("errors");
  });
});
