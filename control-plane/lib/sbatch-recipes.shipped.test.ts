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
});
