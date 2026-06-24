import { describe, it, expect } from "vitest";
import { parseRecipe } from "./sbatch-recipes";

const withVars = (varsYaml: string) =>
  [
    "---",
    "name: T",
    "engine: ollama",
    "model_type: chat",
    "api_model_name: m",
    "port: 8000",
    varsYaml,
    "---",
    "apptainer exec {{image}} serve",
  ].join("\n");

describe("recipe variables parsing", () => {
  it("parses a variables block with defaults and required flags", () => {
    const r = parseRecipe(
      "t",
      withVars(
        ["variables:", "  - name: image", "    label: Image", "    default: /x.sif", "    required: true"].join("\n"),
      ),
    );
    expect(r.valid).toBe(true);
    expect(r.header?.variables).toEqual([
      { name: "image", label: "Image", default: "/x.sif", required: true },
    ]);
  });

  it("defaults `required` to false and allows no label/default", () => {
    const r = parseRecipe("t", withVars(["variables:", "  - name: tag"].join("\n")));
    expect(r.header?.variables).toEqual([{ name: "tag", required: false }]);
  });

  it("rejects an invalid variable name", () => {
    const r = parseRecipe("t", withVars(["variables:", "  - name: 9bad"].join("\n")));
    expect(r.valid).toBe(false);
    expect(r.error).toMatch(/variable/i);
  });

  it("rejects duplicate variable names", () => {
    const r = parseRecipe(
      "t",
      withVars(["variables:", "  - name: a", "  - name: a"].join("\n")),
    );
    expect(r.valid).toBe(false);
    expect(r.error).toMatch(/duplicate/i);
  });

  it("treats a missing variables block as no variables", () => {
    const r = parseRecipe("t", withVars("description: none"));
    expect(r.valid).toBe(true);
    expect(r.header?.variables).toBeUndefined();
  });
});
