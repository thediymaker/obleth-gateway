import { describe, it, expect } from "vitest";
import { parseRecipe, buildManagedFromRecipe } from "./sbatch-recipes";

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

const recipeWithBody = (body: string) =>
  parseRecipe(
    "t",
    [
      "---", "name: T", "engine: ollama", "model_type: chat",
      "api_model_name: m", "port: 8000",
      "variables:", "  - name: image", "    default: /default.sif",
      "  - name: tag", "    required: true",
      "---", body,
    ].join("\n"),
  );

describe("recipe variable substitution", () => {
  it("substitutes declared {{vars}} and leaves shell ${} and undeclared {{}} alone", () => {
    const r = recipeWithBody("run {{image}} --tag {{tag}} --home ${HOME} --x {{undeclared}}");
    const p = buildManagedFromRecipe(r, { variables: { image: "/my.sif", tag: "q4" } });
    expect(p.managedBody.script_body).toContain("run /my.sif --tag q4");
    expect(p.managedBody.script_body).toContain("${HOME}");
    expect(p.managedBody.script_body).toContain("{{undeclared}}");
  });

  it("falls back to the declared default when a value is omitted", () => {
    const r = recipeWithBody("run {{image}} {{tag}}");
    const p = buildManagedFromRecipe(r, { variables: { tag: "q4" } });
    expect(p.managedBody.script_body).toContain("run /default.sif q4");
  });

  it("throws when a required variable has no value and no default", () => {
    const r = recipeWithBody("run {{tag}}");
    expect(() => buildManagedFromRecipe(r, { variables: {} })).toThrow(/required/i);
  });

  it("does not re-expand a substituted value that contains another token", () => {
    const r = recipeWithBody("run {{image}} {{tag}}");
    const p = buildManagedFromRecipe(r, { variables: { image: "/x {{tag}}", tag: "q4" } });
    // {{tag}} inside the image value must stay literal, not become q4
    expect(p.managedBody.script_body).toContain("run /x {{tag}} q4");
  });

  it("leaves an optional variable's token in place when unset and undefaulted", () => {
    const r = parseRecipe(
      "t",
      [
        "---", "name: T", "engine: ollama", "model_type: chat",
        "api_model_name: m", "port: 8000",
        "variables:", "  - name: opt",
        "---", "run {{opt}}",
      ].join("\n"),
    );
    const p = buildManagedFromRecipe(r, { variables: {} });
    expect(p.managedBody.script_body).toContain("run {{opt}}");
  });
});
