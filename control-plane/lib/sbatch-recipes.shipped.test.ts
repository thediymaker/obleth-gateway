import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { parseRecipe } from "./sbatch-recipes";

// Validates the recipe files we actually ship in control-plane/recipes/. The
// other suites parse inline/temp fixtures, so a YAML mistake in a bundled
// recipe (e.g. an unquoted description containing a colon) would otherwise only
// surface at runtime in the gallery. This parses each shipped file directly.
describe("shipped recipe files", () => {
  const dir = path.join(process.cwd(), "recipes");
  const files = readdirSync(dir).filter((f) => f.endsWith(".recipe"));

  it("ships at least one recipe", () => {
    expect(files.length).toBeGreaterThan(0);
  });

  it.each(files)("%s parses as a valid recipe", (file) => {
    const text = readFileSync(path.join(dir, file), "utf8");
    const parsed = parseRecipe(file.replace(/\.recipe$/, ""), text);
    expect(parsed.valid, parsed.error ?? "invalid recipe").toBe(true);
  });

  // A flag that starts a tool parser must say so, or the model it launches
  // isn't flagged for function calling and its tool boons stay off.
  it.each(files)("%s declares function calling on any flag that turns tools on", (file) => {
    const parsed = parseRecipe(file.replace(/\.recipe$/, ""), readFileSync(path.join(dir, file), "utf8"));
    for (const input of parsed.header?.inputs ?? []) {
      if (input.type === "flag" && /--tool-call-parser|--enable-auto-tool-choice/.test(input.adds ?? "")) {
        expect(input.capabilities, `${input.name} adds a tool parser`).toEqual(expect.arrayContaining(["function_calling", "tool_choice"]));
      }
    }
  });

  // The library is a catalog of recipes that are known to work: each one says
  // where it was run and served, so nothing untried ships.
  it.each(files)("%s says where it was run and served", (file) => {
    const parsed = parseRecipe(file.replace(/\.recipe$/, ""), readFileSync(path.join(dir, file), "utf8"));
    const tested = parsed.header?.tested ?? [];
    expect(tested.length, "add a tested: entry after running it").toBeGreaterThan(0);
    for (const t of tested) {
      expect(t.hardware).toBeTruthy();
      expect(t.date).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    }
  });
});
