import { describe, it, expect } from "vitest";
import { readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { spawnSync } from "node:child_process";
import { buildDeployPreview, buildManagedFromRecipe, HEADER_KEYS, parseRecipe } from "./sbatch-recipes";
import { EMPTY_CLUSTER } from "./recipe-inputs";

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

// Every shipped recipe has one shape, so a person who has read one can read
// them all, and each behaves the same way under Slurm.
describe("shipped recipes share one shape", () => {
  const dir = path.join(process.cwd(), "recipes");
  const files = readdirSync(dir).filter((f) => f.endsWith(".recipe"));
  const load = (file: string) => {
    const text = readFileSync(path.join(dir, file), "utf8");
    return { text, parsed: parseRecipe(file.replace(/\.recipe$/, ""), text) };
  };
  const LABELS: Record<string, string> = { context: "Context length", parallel: "Requests at once", tools: "Tool calling", reasoning: "Reasoning", image: "Container image", cache: "Weight cache" };
  const ORDER = ["model", "context", "parallel", "extra", "image", "cache"];

  it.each(files)("%s has no warnings", (file) => {
    expect(load(file).parsed.warnings).toEqual([]);
  });

  it.each(files)("%s writes its header keys in the shared order", (file) => {
    const keys = [...load(file).text.split("\n---\n")[0].matchAll(/^([a-z_]+):/gm)].map((m) => m[1]);
    const order = keys.map((k) => HEADER_KEYS.indexOf(k as (typeof HEADER_KEYS)[number]));
    expect(order).toEqual([...order].sort((a, b) => a - b));
    for (const k of ["health_path", "target_replicas", "max_job_failures"]) expect(keys).toContain(k);
  });

  it.each(files)("%s names and orders its inputs the shared way", (file) => {
    const inputs = load(file).parsed.header!.inputs;
    for (const i of inputs) if (LABELS[i.name] && !(i.name === "context" && i.label === "Longest input")) expect(i.label, i.name).toBe(LABELS[i.name]);
    const names = inputs.map((i) => i.name);
    expect(names.slice(-2)).toEqual(["image", "cache"]);
    const at = names.filter((n) => ORDER.includes(n)).map((n) => ORDER.indexOf(n));
    expect(at).toEqual([...at].sort((a, b) => a - b));
    if (load(file).parsed.header!.engine !== "ollama") expect(names).toContain("extra");
    expect(inputs.find((i) => i.name === "image")?.required).toBe(false);
  });

  it.each(files)("%s runs setup before strict mode, execs its server, and serves on the replica's port under the deployment's name", (file) => {
    const body = load(file).parsed.body!;
    const setup = body.indexOf("{{cluster.setup}}");
    expect(setup).toBeGreaterThan(-1);
    expect(setup).toBeLessThan(body.indexOf("set -euo pipefail"));
    const code = body.split("\n").filter((l) => !/^\s*#/.test(l)).join("\n");
    expect(code).toMatch(/^exec /m);
    expect(code).toContain("0.0.0.0");
    expect(code).toContain("$OBLETH_SERVING_PORT");
    expect(code).toContain("{{api_model_name}}");
    // The container command is the cluster's choice, with apptainer as the fallback.
    expect(code.replace(/\$\{OBLETH_CONTAINER_RUN:-apptainer exec --nv\}/g, "")).not.toMatch(/apptainer|singularity/);
  });

  it.each(files)("%s gives the model the context window it serves", (file) => {
    const { parsed } = load(file);
    if (parsed.header!.model_type === "audio_transcription") return;
    expect(buildDeployPreview(parsed)?.contextWindow, "add a context input or context_window").toBeGreaterThan(0);
  });

  it.each(files)("%s renders to a script bash can parse, with and without an image", (file) => {
    const { parsed } = load(file);
    const cluster = { ...EMPTY_CLUSTER, cache: "/shared/cache", setup: "module load x", image: { vllm: "/i/v.sif", sglang: "/i/s.sif", llamacpp: "/i/l.sif", ollama: "/i/o.sif" } };
    for (const image of [undefined, ""]) {
      const inputs = { ...(parsed.header!.inputs.some((i) => i.name === "model" && !i.default) ? { model: "org/model" } : {}), ...(image === undefined ? {} : { image }) };
      const script = buildManagedFromRecipe(parsed, { inputs }, cluster).managedBody.script_body!;
      const res = spawnSync("bash", ["-n"], { input: script, encoding: "utf8" });
      expect(res.status, res.stderr).toBe(0);
    }
  });
});
