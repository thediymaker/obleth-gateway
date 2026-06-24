import { describe, it, expect } from "vitest";
import { toRecipeCards } from "./recipe-card";
import type { ParsedRecipe } from "@/lib/sbatch-recipes";

const valid: ParsedRecipe = {
  id: "glm",
  valid: true,
  warnings: ["--job-name (not applied)"],
  header: {
    name: "GLM-5.2",
    engine: "llamacpp",
    model_type: "chat",
    api_model_name: "glm-5.2",
    description: "weights on NVMe",
    port: 8000,
    target_replicas: 4,
    max_job_failures: 3,
  },
  body: "#!/bin/bash\nllama-server",
  directives: { warnings: [] },
};

const invalid: ParsedRecipe = {
  id: "broken",
  valid: false,
  error: "recipe has no script body",
  warnings: [],
};

describe("toRecipeCards", () => {
  it("maps a valid recipe's header fields onto a flat card", () => {
    const [card] = toRecipeCards([valid]);
    expect(card).toEqual({
      id: "glm",
      valid: true,
      error: undefined,
      name: "GLM-5.2",
      engine: "llamacpp",
      modelType: "chat",
      description: "weights on NVMe",
      apiModelName: "glm-5.2",
      targetReplicas: 4,
      warnings: ["--job-name (not applied)"],
      source: "file",
    });
  });

  it("carries an invalid recipe through with its error and undefined header fields", () => {
    const [card] = toRecipeCards([invalid]);
    expect(card.valid).toBe(false);
    expect(card.error).toBe("recipe has no script body");
    expect(card.name).toBeUndefined();
    expect(card.warnings).toEqual([]);
    expect(card.source).toBe("file");
  });
});
