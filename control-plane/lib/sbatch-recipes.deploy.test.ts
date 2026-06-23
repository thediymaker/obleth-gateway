import { describe, it, expect } from "vitest";
import { parseRecipe, buildManagedFromRecipe } from "./sbatch-recipes";

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
});
