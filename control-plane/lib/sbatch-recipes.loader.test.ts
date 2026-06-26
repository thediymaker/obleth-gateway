import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { listRecipes, listRecipeDocs, getRecipe, loadRecipeCards, resolveRecipeById } from "./sbatch-recipes";
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

describe("listRecipeDocs", () => {
  it("returns raw text paired with id for each *.recipe file", () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    const docs = listRecipeDocs();
    expect(docs).toHaveLength(1);
    expect(docs[0].id).toBe("glm");
    expect(docs[0].text).toBe(VALID);
  });

  it("returns [] when the directory is absent", () => {
    process.env.OBLETH_RECIPES_DIR = path.join(dir, "does-not-exist");
    expect(listRecipeDocs()).toEqual([]);
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

  it("file card body is the full raw document (starts with ---), not just the script", async () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    const cards = await loadRecipeCards();
    const fileCard = cards.find((c) => c.id === "glm");
    expect(fileCard?.body).toBeDefined();
    expect(fileCard?.body?.startsWith("---")).toBe(true);
    expect(fileCard?.body).toBe(VALID);
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

  it("DB card name uses the saved Template name over the frontmatter name", async () => {
    // DB_ROW_BODY has frontmatter `name: Qwen2.5`, but the row's Template name
    // is "Qwen2.5 DB". The saved Template name must win for the card label.
    const cards = await loadRecipeCards();
    const dbCard = cards.find((c) => c.recipeId === "db-qwen");
    expect(dbCard?.name).toBe("Qwen2.5 DB");
  });

  it("DB card falls back to the frontmatter name when the row name is empty", async () => {
    vi.mocked(obleth.listRecipes).mockResolvedValue([
      { id: "db-qwen", name: "", body: DB_ROW_BODY, author: "admin" },
    ]);
    const cards = await loadRecipeCards();
    const dbCard = cards.find((c) => c.recipeId === "db-qwen");
    expect(dbCard?.name).toBe("Qwen2.5");
  });
});

describe("resolveRecipeById (file-first, then DB)", () => {
  const DB_BODY = [
    "---",
    "name: From DB",
    "engine: vllm",
    "model_type: chat",
    "api_model_name: from-db",
    "port: 8000",
    "---",
    "#!/bin/bash -l",
    "vllm serve",
  ].join("\n");

  afterEach(() => {
    vi.clearAllMocks();
  });

  it("resolves a file recipe by id without touching the admin API", async () => {
    writeFileSync(path.join(dir, "glm.recipe"), VALID);
    vi.mocked(obleth.listRecipes).mockResolvedValue([]);
    const r = await resolveRecipeById("glm");
    expect(r?.valid).toBe(true);
    expect(r?.header?.api_model_name).toBe("glm");
    expect(obleth.listRecipes).not.toHaveBeenCalled();
  });

  it("resolves a DB recipe by UUID id when no file matches", async () => {
    vi.mocked(obleth.listRecipes).mockResolvedValue([
      { id: "5ae0a150-32e3-4af9-8630-3a22a905450c", name: "Saved", body: DB_BODY, author: "admin" },
    ]);
    const r = await resolveRecipeById("5ae0a150-32e3-4af9-8630-3a22a905450c");
    expect(r?.valid).toBe(true);
    expect(r?.header?.api_model_name).toBe("from-db");
  });

  it("returns null when neither a file nor a DB row matches", async () => {
    vi.mocked(obleth.listRecipes).mockResolvedValue([]);
    expect(await resolveRecipeById("nope")).toBeNull();
  });

  it("returns null (not throw) when the admin API is down and no file matches", async () => {
    vi.mocked(obleth.listRecipes).mockRejectedValue(new Error("API unavailable"));
    expect(await resolveRecipeById("nope")).toBeNull();
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
