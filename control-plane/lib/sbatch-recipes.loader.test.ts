import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { listRecipes, getRecipe, loadRecipeCards } from "./sbatch-recipes";
import { obleth } from "@/lib/obleth";

vi.mock("@/lib/obleth", () => ({
  obleth: {
    listRecipes: vi.fn(),
  },
}));

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

  it("returns entries sorted by id regardless of disk order", () => {
    // Write files out of alphabetical order
    const zebraRecipe = VALID.replace("name: GLM", "name: Zebra").replace("api_model_name: glm", "api_model_name: zebra");
    const aardvarkRecipe = VALID.replace("name: GLM", "name: Aardvark").replace("api_model_name: glm", "api_model_name: aardvark");
    writeFileSync(path.join(dir, "zebra.recipe"), zebraRecipe);
    writeFileSync(path.join(dir, "aardvark.recipe"), aardvarkRecipe);
    const recipes = listRecipes();
    expect(recipes.map((r) => r.id)).toEqual(["aardvark", "zebra"]);
  });
});

describe("loadRecipeCards merge (file + DB)", () => {
  const DB_ROW_BODY = [
    "---",
    "name: Qwen2.5",
    "engine: ollama",
    "model_type: chat",
    "api_model_name: qwen2.5",
    "port: 11434",
    "partition: gpu",
    "---",
    "#!/bin/bash -l",
    "#SBATCH -p gpu",
    "ollama serve",
  ].join("\n");

  beforeEach(() => {
    vi.mocked(obleth.listRecipes).mockResolvedValue([
      { id: "db-qwen", name: "Qwen2.5 DB", body: DB_ROW_BODY, author: "admin" },
    ]);
  });

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("file cards have source:file", async () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    const cards = await loadRecipeCards();
    const fileCard = cards.find((c) => c.id === "glm");
    expect(fileCard?.source).toBe("file");
    expect(fileCard?.recipeId).toBeUndefined();
  });

  it("DB rows produce source:db cards with recipeId and a parsed preview", async () => {
    const cards = await loadRecipeCards();
    const dbCard = cards.find((c) => c.recipeId === "db-qwen");
    expect(dbCard?.source).toBe("db");
    expect(dbCard?.recipeId).toBe("db-qwen");
    expect(dbCard?.preview).toBeDefined();
    expect(dbCard?.preview?.engine).toBe("ollama");
    expect(dbCard?.preview?.port).toBe(11434);
  });

  it("returns file cards when admin API throws", async () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    vi.mocked(obleth.listRecipes).mockRejectedValue(new Error("API unavailable"));
    const cards = await loadRecipeCards();
    expect(cards.every((c) => c.source === "file")).toBe(true);
    expect(cards.some((c) => c.id === "glm")).toBe(true);
  });

  it("merges file and DB cards in order: files first, then DB", async () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    const cards = await loadRecipeCards();
    const sources = cards.map((c) => c.source);
    const firstDbIdx = sources.indexOf("db");
    const lastFileIdx = sources.lastIndexOf("file");
    // All file cards appear before any DB card (or there are only file/only db cards)
    if (firstDbIdx !== -1 && lastFileIdx !== -1) {
      expect(lastFileIdx).toBeLessThan(firstDbIdx);
    }
  });
});
