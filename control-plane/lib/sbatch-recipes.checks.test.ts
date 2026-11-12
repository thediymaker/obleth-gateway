import { describe, it, expect } from "vitest";
import { buildManagedFromRecipe, buildDeployPreview, parseRecipe, scriptDigest } from "./sbatch-recipes";
import { EMPTY_CLUSTER, inputCapabilities, renderScript, scriptCapabilities } from "./recipe-inputs";

const SERVE = 'vllm serve m --served-model-name {{api_model_name}} --port "$OBLETH_SERVING_PORT"';

const recipe = (header: string[] = [], body: string[] = [SERVE]) =>
  ["---", "name: T", "engine: vllm", "model_type: chat", "api_model_name: t", ...header, "---", "#!/bin/bash", ...body].join("\n");

describe("the loader refuses what it can't apply", () => {
  it("names an unknown header key and suggests the right one", () => {
    const r = parseRecipe("t", recipe(["time: 2-00:00:00", "health-path: /x"]));
    expect(r.valid).toBe(false);
    expect(r.error).toContain('"time" isn\'t a recipe setting; did you mean time_limit?');
    expect(r.error).toContain("did you mean health_path?");
  });

  it("names an unknown key on an input", () => {
    const r = parseRecipe("t", recipe(["inputs:", "  - name: ctx", "    type: choice", "    option: [1, 2]"], [`${SERVE} {{ctx}}`]));
    expect(r.valid).toBe(false);
    expect(r.error).toContain("inputs.0 (ctx)");
    expect(r.error).toContain("did you mean options?");
  });

  it("reads required: no and \"false\" as false, and refuses other words", () => {
    const no = parseRecipe("t", recipe(["inputs:", "  - name: a", "    required: no", "  - name: b", '    required: "false"'], [`${SERVE} {{a}} {{b}}`]));
    expect(no.valid, no.error).toBe(true);
    expect(no.header!.inputs.map((i) => i.required)).toEqual([false, false]);
    const bad = parseRecipe("t", recipe(["inputs:", "  - name: a", "    required: maybe"], [`${SERVE} {{a}}`]));
    expect(bad.valid).toBe(false);
  });

  it("refuses a choice whose default isn't an option, and a flag with no adds", () => {
    expect(parseRecipe("t", recipe(["inputs:", "  - name: c", "    type: choice", "    options: [1, 2]", "    default: 3"], [`${SERVE} {{c}}`])).error).toContain("isn't one of the options");
    expect(parseRecipe("t", recipe(["inputs:", "  - name: f", "    type: flag"], [`${SERVE} {{f}}`])).error).toContain("a flag needs adds");
  });

  it("refuses a script token no input declares, and an unknown cluster default", () => {
    expect(parseRecipe("t", recipe([], [`${SERVE} {{modle}}`])).error).toContain("no input is called modle");
    expect(parseRecipe("t", recipe([], [`${SERVE} {{cluster.cahce}}`])).error).toContain("{{cluster.cahce}} isn't a cluster default");
  });

  it("refuses a script that doesn't listen on $OBLETH_SERVING_PORT or uses a fixed port", () => {
    expect(parseRecipe("t", recipe([], ["vllm serve m"])).error).toContain("never uses $OBLETH_SERVING_PORT");
    expect(parseRecipe("t", recipe([], ['vllm serve m --port 8000 # not "$OBLETH_SERVING_PORT"'])).error).toContain("fixed port 8000");
  });

  it("refuses #SBATCH lines it can't send, and says what to use instead", () => {
    const r = parseRecipe("t", recipe([], ["#SBATCH --mem-per-gpu=80G", SERVE]));
    expect(r.valid).toBe(false);
    expect(r.error).toContain("--mem-per-gpu=80G can't be applied; use --mem");
    const gpus = parseRecipe("t", recipe([], ["#SBATCH -G 2", SERVE]));
    expect(gpus.valid).toBe(true);
    expect(gpus.directives?.gres).toBe("gpu:2");
  });

  it("refuses a {{token}} in an env value", () => {
    expect(parseRecipe("t", recipe(["env:", '  HF_HOME: "{{cache}}"'])).error).toContain("env values are exported exactly as written");
  });

  it("refuses a served name the engine or shell would read as two words", () => {
    expect(parseRecipe("t", recipe([]).replace("api_model_name: t", "api_model_name: Gemma 4")).valid).toBe(false);
    const ok = parseRecipe("t", recipe([]));
    expect(() => buildManagedFromRecipe(ok, { api_model_name: "Gemma 4" })).toThrow(/can't be used/);
  });

  it("warns about an input the script never uses, and normalises engine aliases", () => {
    const r = parseRecipe("t", recipe(["inputs:", "  - name: unused"]).replace("engine: vllm", "engine: llama.cpp"));
    expect(r.valid, r.error).toBe(true);
    expect(r.header!.engine).toBe("llamacpp");
    expect(r.warnings.join(" ")).toContain("The input unused isn't used in the script");
  });

  it("defaults to one replica, two failed launches and port window 8000", () => {
    const h = parseRecipe("t", recipe()).header!;
    expect([h.target_replicas, h.max_job_failures, h.port]).toEqual([1, 2, 8000]);
  });
});

describe("what the launch gives the model", () => {
  it("sets the context window from the context input, and follows a change", () => {
    const r = parseRecipe("t", recipe(["inputs:", "  - name: context", "    type: number", "    default: 32768"], [`${SERVE} --max-model-len {{context}}`]));
    expect(buildManagedFromRecipe(r).createBody.context_window).toBe(32768);
    expect(buildManagedFromRecipe(r, { inputs: { context: "131072" } }).createBody.context_window).toBe(131072);
    expect(buildDeployPreview(r)?.contextWindow).toBe(32768);
  });

  it("uses a fixed context_window, and leaves it out when nothing says", () => {
    expect(buildManagedFromRecipe(parseRecipe("t", recipe(["context_window: 4096"]))).createBody.context_window).toBe(4096);
    expect(buildManagedFromRecipe(parseRecipe("t", recipe())).createBody).not.toHaveProperty("context_window");
  });

  it("turns on tools from flags typed into the extras, and vision from the header unless a flag drops it", () => {
    const r = parseRecipe("t", recipe(
      ["capabilities: [vision]", "inputs:", "  - name: extra", "  - name: text_only", "    type: flag", "    adds: --language-model-only", "    drops: [vision]"],
      [`${SERVE} {{text_only}} {{extra}}`],
    ));
    expect(r.valid, r.error).toBe(true);
    const on = buildManagedFromRecipe(r, { inputs: { extra: "--enable-auto-tool-choice --tool-call-parser hermes" } }).createBody;
    expect(on).toMatchObject({ supports_function_calling: true, supports_tool_choice: true, supports_vision: true });
    const textOnly = buildManagedFromRecipe(r, { inputs: { text_only: "true" } }).createBody;
    expect(textOnly).not.toHaveProperty("supports_vision");
    expect(textOnly).not.toHaveProperty("supports_function_calling");
  });

  it("reads llama-server --jinja as tool calls, but not a commented-out flag", () => {
    expect(scriptCapabilities("llama-server -m x --jinja")).toEqual(["function_calling", "tool_choice"]);
    expect(scriptCapabilities("# --tool-call-parser hermes\nvllm serve x")).toEqual([]);
    expect(inputCapabilities([], {}, EMPTY_CLUSTER, 1, { always: ["vision"] })).toEqual(["vision"]);
  });

  it("takes the model type from the launch when given", () => {
    expect(buildManagedFromRecipe(parseRecipe("t", recipe()), { model_type: "embedding" }).createBody.model_type).toBe("embedding");
  });

  it("keeps a script edited at launch and a digest of what was submitted", () => {
    const r = parseRecipe("t", recipe());
    const edited = `#!/bin/bash\n${SERVE} --marker`;
    const spec = buildManagedFromRecipe(r, { script_body: edited }).managedBody;
    expect(spec.launcher_spec).toMatchObject({ script: edited, script_digest: scriptDigest(spec.script_body ?? "") });
    expect(buildManagedFromRecipe(r).managedBody.launcher_spec).not.toHaveProperty("script");
  });
});

describe("rendering", () => {
  const inputs = [{ name: "image", type: "path" as const, required: false, default: "/default.sif" }, { name: "on", type: "flag" as const, required: false, default: "true", adds: "--on" }];

  it("keeps an input cleared on purpose empty, but a cleared flag follows its default", () => {
    expect(renderScript("run [{{image}}] {{on}}", inputs, { image: "", on: "" }, EMPTY_CLUSTER)).toBe("run [] --on");
    expect(renderScript("run [{{image}}]", inputs, {}, EMPTY_CLUSTER)).toBe("run [/default.sif]");
  });

  it("fills tokens written with spaces, and refuses leftovers when strict", () => {
    expect(renderScript("run {{ image }}", inputs, {}, EMPTY_CLUSTER)).toBe("run /default.sif");
    expect(() => renderScript("run {{nope}}", inputs, {}, EMPTY_CLUSTER, { strict: true })).toThrow(/no input of the recipe fills/);
    expect(renderScript("docker ps --format '{{.ID}}'", inputs, {}, EMPTY_CLUSTER, { strict: true })).toContain("{{.ID}}");
  });
});
