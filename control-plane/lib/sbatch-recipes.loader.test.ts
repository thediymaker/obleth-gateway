import { describe, it, expect, beforeEach, afterEach } from "vitest";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { listRecipes, getRecipe } from "./sbatch-recipes";

const VALID = [
  "---",
  "name: GLM",
  "engine: llamacpp",
  "model_type: chat",
  "api_model_name: glm",
  "port: 8000",
  "---",
  "#!/bin/bash -l",
  "#SBATCH -p arm",
  "llama-server -hf unsloth/GLM-5.2-GGUF:UD-IQ2_M --port 8000",
].join("\n");

let dir: string;
beforeEach(() => {
  dir = mkdtempSync(path.join(tmpdir(), "recipes-"));
  process.env.OBLETH_RECIPES_DIR = dir;
});
afterEach(() => {
  delete process.env.OBLETH_RECIPES_DIR;
  rmSync(dir, { recursive: true, force: true });
});

describe("listRecipes / getRecipe", () => {
  it("lists *.recipe files and ignores other files", () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    writeFileSync(path.join(dir, "notes.txt"), "ignore me");
    writeFileSync(path.join(dir, "wizard.yaml"), "id: x"); // legacy wizard def, not ours
    const recipes = listRecipes();
    expect(recipes.map((r) => r.id)).toEqual(["glm"]);
    expect(recipes[0].valid).toBe(true);
  });

  it("returns an invalid entry instead of throwing on a bad file", () => {
    writeFileSync(path.join(dir, "broken.recipe"), "name: only-a-header-no-fence");
    const recipes = listRecipes();
    expect(recipes).toHaveLength(1);
    expect(recipes[0].valid).toBe(false);
    expect(recipes[0].error).toBeTruthy();
  });

  it("returns [] when the directory is absent", () => {
    process.env.OBLETH_RECIPES_DIR = path.join(dir, "does-not-exist");
    expect(listRecipes()).toEqual([]);
  });

  it("getRecipe loads one by id, null when missing", () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    expect(getRecipe("glm")?.header?.api_model_name).toBe("glm");
    expect(getRecipe("nope")).toBeNull();
  });
});
