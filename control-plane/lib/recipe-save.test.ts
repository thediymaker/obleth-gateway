import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import path from "node:path";
import { baselineForm, setField } from "./deploy-form";
import { EMPTY_CLUSTER, inputDefaults } from "./recipe-inputs";
import { savedName, savedRecipeText } from "./recipe-save";
import { buildDeployPreview, parseRecipe } from "./sbatch-recipes";

const source = readFileSync(path.join(process.cwd(), "recipes", "nemotron-3-ultra-gh200.recipe"), "utf8");
const cv = { ...EMPTY_CLUSTER, cache: "/scratch/hf", image: { vllm: "/scratch/vllm.sif" } };

describe("save as recipe", () => {
  const recipe = parseRecipe("nemotron-3-ultra-gh200", source);
  const base = baselineForm(recipe.id, buildDeployPreview(recipe)!, cv);
  const form = setField(setField(setField(setField(base, "slurm.partition", "gh200"), "slurm.nodes", 4), "inputs.offload_gb", "10"), "slurm.qos", "long");

  it("writes the values used as defaults and keeps cluster references", () => {
    const text = savedRecipeText(source, form, { name: savedName("Nemotron 3 Ultra on Grace Hopper", form), basedOn: recipe.id, clusterResolved: inputDefaults(recipe.header!.inputs, cv, 4) });
    const saved = parseRecipe("saved", text);
    expect(saved.valid).toBe(true);
    const h = saved.header!;
    expect(h).toMatchObject({ name: "Nemotron 3 Ultra on Grace Hopper · gh200 · 4 nodes", based_on: "nemotron-3-ultra-gh200", partition: "gh200", qos: "long", nodes: 4 });
    expect(h.inputs.find((i) => i.name === "offload_gb")).toMatchObject({ default: "10", by_nodes: { "1": "270", "4": "10", "8": "0" } });
    expect(h.inputs.find((i) => i.name === "image")?.default).toBe("{{cluster.image.vllm}}");
    expect(saved.body).toBe(recipe.body);
    expect(text).toContain("# First start downloads 352 GB");
  });
});
