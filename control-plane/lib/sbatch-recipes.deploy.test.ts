import { describe, it, expect } from "vitest";
import { parseRecipe, buildManagedFromRecipe, buildDeployPreview } from "./sbatch-recipes";

const file = (extraHeader = "", body?: string) =>
  [
    "---",
    "name: GLM",
    "engine: llamacpp",
    "model_type: chat",
    "api_model_name: glm-5.2",
    "port: 8000",
    extraHeader,
    "---",
    body ??
      ["#!/bin/bash -l", "#SBATCH --gres=gpu:1   # comment", "#SBATCH -p arm", "llama-server -hf repo:Q4 --port 8000"].join("\n"),
  ]
    .filter(Boolean)
    .join("\n");

describe("buildManagedFromRecipe", () => {
  it("maps header + directives onto create/managed bodies", () => {
    const p = buildManagedFromRecipe(parseRecipe("glm", file()));
    expect(p.createBody).toMatchObject({
      model_name: "glm-5.2",
      model_type: "chat",
      api_base: "",
    });
    expect(p.managedBody).toMatchObject({
      enabled: true,
      partition: "arm",
      gres: "gpu:1",
      serving_port: 8000,
      health_path: "/health",
      target_replicas: 2,
      max_job_failures: 3,
      launch_command: "",
      image: "",
    });
    expect(p.managedBody.script_body).toContain("llama-server -hf repo:Q4");
  });

  it("applies api_model_name and target_replicas overrides", () => {
    const p = buildManagedFromRecipe(parseRecipe("glm", file()), {
      api_model_name: "glm-test",
      target_replicas: 4,
    });
    expect(p.createBody.model_name).toBe("glm-test");
    expect(p.managedBody.target_replicas).toBe(4);
  });

  it("applies qos/partition/time_limit overrides over the recipe directives", () => {
    // recipe parses partition=arm from `#SBATCH -p arm`, no qos/time
    const p = buildManagedFromRecipe(parseRecipe("glm", file()), {
      qos: "private",
      partition: "gpu",
      time_limit: "02:00:00",
    });
    expect(p.managedBody.qos).toBe("private");
    expect(p.managedBody.partition).toBe("gpu");
    expect(p.managedBody.time_limit).toBe("02:00:00");
  });

  it("keeps the recipe value when an override field is omitted", () => {
    const p = buildManagedFromRecipe(parseRecipe("glm", file()), { qos: "private" });
    expect(p.managedBody.qos).toBe("private");
    expect(p.managedBody.partition).toBe("arm"); // untouched, from `#SBATCH -p arm`
  });

  it("clears a field when its override is empty/whitespace", () => {
    const recipe = parseRecipe("glm", file("qos: private"));
    expect(buildManagedFromRecipe(recipe).managedBody.qos).toBe("private");
    expect(buildManagedFromRecipe(recipe, { qos: "   " }).managedBody.qos).toBeNull();
  });

  it("prepends a cd guard when the recipe declares --chdir", () => {
    const p = buildManagedFromRecipe(
      parseRecipe(
        "glm",
        file("", ["#!/bin/bash -l", "#SBATCH --chdir=/scratch/run", "llama-server --port 8000"].join("\n")),
      ),
    );
    const lines = (p.managedBody.script_body ?? "").split("\n");
    expect(lines[0]).toBe("#!/bin/bash -l");
    expect(lines[1]).toBe("cd '/scratch/run' || exit 1");
  });

  it("records the recipe id in launcher_spec metadata", () => {
    const p = buildManagedFromRecipe(parseRecipe("glm", file()));
    expect(p.managedBody.launcher_spec).toMatchObject({ source: "recipe", recipe_id: "glm" });
  });

  it("stores the engine in launcher_spec and ollama gets / health default", () => {
    const p = buildManagedFromRecipe(
      parseRecipe(
        "olm",
        [
          "---",
          "name: O",
          "engine: ollama",
          "model_type: chat",
          "api_model_name: o",
          "port: 11434",
          "---",
          "ollama serve",
        ].join("\n"),
      ),
    );
    expect(p.managedBody.health_path).toBe("/");
    expect(p.managedBody.launcher_spec).toMatchObject({ engine: "ollama" });
  });

  it("throws on an invalid recipe", () => {
    expect(() => buildManagedFromRecipe({ id: "x", valid: false, error: "bad", warnings: [] })).toThrow();
  });

  it("escapes single quotes in chdir path", () => {
    const p = buildManagedFromRecipe(
      parseRecipe(
        "glm",
        file("", ["#!/bin/bash -l", "#SBATCH --chdir=/data/o'brien", "llama-server --port 8000"].join("\n")),
      ),
    );
    const lines = (p.managedBody.script_body ?? "").split("\n");
    expect(lines[0]).toBe("#!/bin/bash -l");
    expect(lines[1]).toBe("cd '/data/o'\\''brien' || exit 1");
  });
});

describe("buildDeployPreview", () => {
  it("mirrors the deployed managed body (header + parsed placement)", () => {
    const p = buildDeployPreview(parseRecipe("glm", file()));
    expect(p).toMatchObject({
      apiModelName: "glm-5.2",
      modelType: "chat",
      engine: "llamacpp",
      port: 8000,
      healthPath: "/health",
      targetReplicas: 2,
      maxJobFailures: 3,
      partition: "arm",
      gres: "gpu:1",
    });
    expect(p?.scriptBody).toContain("llama-server -hf repo:Q4");
  });

  it("returns undefined for an invalid recipe", () => {
    const p = buildDeployPreview(parseRecipe("x", "no frontmatter here"));
    expect(p).toBeUndefined();
  });
});
