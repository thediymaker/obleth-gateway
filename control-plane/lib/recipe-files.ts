// File-based launch recipes (Open OnDemand-style).
//
// Recipes are admin-authored YAML files, *not* database rows: an operator drops
// a `<name>.yaml` into the recipes directory (default `control-plane/recipes/`,
// override with `OBLETH_RECIPES_DIR`) describing the executable/module/container
// and the knobs the launch wizard should expose. They are read on the server and
// passed into the wizard as data — the admin owns them and they don't change at
// launch time.
//
// This module is server-only (it touches the filesystem). The pure command
// composer lives in `model-recipes.ts` and is shared with the browser.

import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import { SLURM_RECIPES, type SlurmRecipe } from "./model-recipes";

const ArgSchema = z
  .object({
    flag: z.string().optional(),
    flags: z.array(z.string()).optional(),
    omit_when: z.array(z.coerce.string()).optional(),
    switch: z.string().optional(),
    switch_value: z.coerce.string().optional(),
    env: z.string().optional(),
    env_value: z.coerce.string().optional(),
    raw: z.boolean().optional(),
  })
  .strict();

const ParamSchema = z
  .object({
    id: z.string().min(1),
    label: z.string().min(1),
    kind: z.enum(["number", "text", "select", "boolean"]),
    default: z.coerce.string().default(""),
    options: z
      .array(z.object({ value: z.coerce.string(), label: z.coerce.string() }))
      .optional(),
    hint: z.string().optional(),
    placeholder: z.string().optional(),
    advanced: z.boolean().optional(),
    min: z.number().optional(),
    max: z.number().optional(),
    step: z.number().optional(),
    steps: z.array(z.number()).optional(),
    arg: ArgSchema.optional(),
  })
  .strict();

const CommandSchema = z
  .object({
    executable: z.string().min(1),
    prefix_args: z.array(z.coerce.string()).optional(),
    model_flag: z.string().optional(),
    fixed_args: z.array(z.coerce.string()).optional(),
    port_flag: z.string().min(1),
    alias_flag: z.string().optional(),
    served_name_flag: z.string().optional(),
  })
  .strict();

const RecipeFileSchema = z
  .object({
    id: z.string().min(1),
    label: z.string().min(1),
    badge: z.string().default(""),
    backend: z.enum(["vllm", "ollama", "llamacpp", "custom"]).optional(),
    hint: z.string().default(""),
    health_path: z.string().default("/health"),
    model_label: z.string().optional(),
    model_placeholder: z.string().optional(),
    model_hint: z.string().optional(),
    image_placeholder: z.string().optional(),
    image_hint: z.string().optional(),
    image_optional: z.boolean().optional(),
    manual: z.boolean().optional(),
    /** Sort key (lower first); ties fall back to filename. */
    order: z.number().optional(),
    params: z.array(ParamSchema).optional(),
    command: CommandSchema.optional(),
    command_template: z.string().optional(),
    env: z.record(z.coerce.string()).optional(),
    preamble: z.string().optional(),
  })
  .strict();

export type RecipeFile = z.infer<typeof RecipeFileSchema>;

/** Map the snake_case file shape onto the camelCase `SlurmRecipe` used by the UI. */
function toRecipe(file: RecipeFile): SlurmRecipe {
  return {
    id: file.id,
    label: file.label,
    badge: file.badge,
    backend: file.backend,
    hint: file.hint,
    healthPath: file.health_path,
    modelLabel: file.model_label,
    modelPlaceholder: file.model_placeholder,
    modelHint: file.model_hint,
    imagePlaceholder: file.image_placeholder,
    imageHint: file.image_hint,
    imageOptional: file.image_optional,
    manual: file.manual,
    params: file.params?.map((p) => ({
      id: p.id,
      label: p.label,
      kind: p.kind,
      default: p.default,
      options: p.options,
      hint: p.hint,
      placeholder: p.placeholder,
      advanced: p.advanced,
      min: p.min,
      max: p.max,
      step: p.step,
      steps: p.steps,
      arg: p.arg
        ? {
            flag: p.arg.flag,
            flags: p.arg.flags,
            omitWhen: p.arg.omit_when,
            switch: p.arg.switch,
            switchValue: p.arg.switch_value,
            env: p.arg.env,
            envValue: p.arg.env_value,
            raw: p.arg.raw,
          }
        : undefined,
    })),
    command: file.command
      ? {
          executable: file.command.executable,
          prefixArgs: file.command.prefix_args,
          modelFlag: file.command.model_flag,
          fixedArgs: file.command.fixed_args,
          portFlag: file.command.port_flag,
          aliasFlag: file.command.alias_flag,
          servedNameFlag: file.command.served_name_flag,
        }
      : undefined,
    commandTemplate: file.command_template,
    env: file.env,
    preamble: file.preamble,
  };
}

/** Parse + validate a single recipe file's YAML text. Throws on invalid input. */
export function parseRecipeFile(text: string): SlurmRecipe {
  return toRecipe(RecipeFileSchema.parse(parseYaml(text)));
}

function recipesDir(): string {
  const override = process.env.OBLETH_RECIPES_DIR?.trim();
  if (override) return path.resolve(override);
  return path.join(process.cwd(), "recipes");
}

/**
 * Load the launch recipes an admin has authored. Reads every `*.yaml`/`*.yml`
 * in the recipes directory; a malformed file is skipped (logged) rather than
 * breaking the whole wizard. The built-in "Custom" manual entry is always
 * appended (unless a file defines its own `custom`). Falls back to the built-in
 * recipes when the directory is missing or empty.
 */
export function loadRecipes(): readonly SlurmRecipe[] {
  const dir = recipesDir();
  let entries: string[];
  try {
    if (!statSync(dir).isDirectory()) return SLURM_RECIPES;
    entries = readdirSync(dir);
  } catch {
    return SLURM_RECIPES; // directory absent — use built-ins
  }

  const files = entries
    .filter((f) => f.endsWith(".yaml") || f.endsWith(".yml"))
    .sort();

  const loaded: { recipe: SlurmRecipe; order: number; name: string }[] = [];
  for (const name of files) {
    const full = path.join(dir, name);
    try {
      const raw = RecipeFileSchema.parse(parseYaml(readFileSync(full, "utf8")));
      loaded.push({ recipe: toRecipe(raw), order: raw.order ?? 100, name });
    } catch (err) {
      console.error(`[recipes] skipping ${name}: ${(err as Error).message}`);
    }
  }

  if (loaded.length === 0) return SLURM_RECIPES;

  loaded.sort((a, b) => a.order - b.order || a.name.localeCompare(b.name));
  const recipes = loaded.map((l) => l.recipe);

  // Guarantee a manual fallback so admins can always hand-write a command.
  if (!recipes.some((r) => r.manual)) {
    const custom = SLURM_RECIPES.find((r) => r.manual);
    if (custom) recipes.push(custom);
  }
  return recipes;
}
