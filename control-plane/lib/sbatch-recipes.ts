// File-based deployment recipes: a YAML metadata header + the raw `sbatch`
// script an admin already tested. The header carries routing metadata (engine,
// model name, port) and optional placement overrides; the body is submitted
// verbatim as `script_body` while its `#SBATCH` directives are lifted into JSON
// fields (slurmrestd ignores `#SBATCH` — see ./sbatch-directives).
//
// Distinct from ./recipe-files.ts (the legacy wizard-definition `*.yaml`
// files); this module owns the new `*.recipe` files and never imports that one.
import { parse as parseYaml } from "yaml";
import { z } from "zod";
import { parseSbatchDirectives, type ParsedDirectives } from "./sbatch-directives";

export interface RecipeHeader {
  name: string;
  description?: string;
  engine: string;
  model_type: string;
  api_model_name: string;
  port: number;
  health_path?: string;
  target_replicas?: number;
  max_job_failures?: number;
  partition?: string;
  gres?: string;
  cpus_per_task?: number;
  mem?: string;
  time_limit?: string;
  nodes?: number;
  account?: string;
  qos?: string;
  constraints?: string;
  exclude?: string;
}

export interface ParsedRecipe {
  id: string;
  valid: boolean;
  error?: string;
  header?: RecipeHeader;
  body?: string;
  directives?: ParsedDirectives;
  warnings: string[];
}

export function defaultHealthPath(engine: string): string {
  return engine === "ollama" ? "/" : "/health";
}

const HeaderSchema = z
  .object({
    name: z.string().min(1),
    description: z.string().optional(),
    engine: z.string().min(1),
    model_type: z.string().min(1),
    api_model_name: z.string().min(1),
    port: z.coerce.number().int().positive(),
    health_path: z.string().optional(),
    target_replicas: z.coerce.number().int().positive().default(2),
    max_job_failures: z.coerce.number().int().nonnegative().default(3),
    partition: z.string().optional(),
    gres: z.string().optional(),
    cpus_per_task: z.coerce.number().int().positive().optional(),
    mem: z.string().optional(),
    time_limit: z.string().optional(),
    nodes: z.coerce.number().int().positive().optional(),
    account: z.string().optional(),
    qos: z.string().optional(),
    constraints: z.string().optional(),
    exclude: z.string().optional(),
  })
  .strip();

/** Split a Jekyll-style `---`\n header \n`---`\n body document. */
function splitFrontmatter(text: string): { header: string; body: string } | null {
  const norm = text.replace(/\r\n/g, "\n");
  if (!norm.startsWith("---\n")) return null;
  // Match a closing fence: exactly "---" on its own line
  const m = norm.slice(4).match(/\n---(?:\r?\n|$)/);
  if (!m || m.index === undefined) return null;
  const fenceStart = 4 + m.index;          // index of the "\n" before "---"
  const header = norm.slice(4, fenceStart);
  const body = norm.slice(fenceStart + m[0].length); // skip past "\n---\n"
  return { header, body };
}

export function parseRecipe(id: string, text: string): ParsedRecipe {
  const split = splitFrontmatter(text);
  if (!split) {
    return { id, valid: false, error: "malformed frontmatter (missing --- fences)", warnings: [] };
  }
  let raw: unknown;
  try {
    raw = parseYaml(split.header);
  } catch (e) {
    return { id, valid: false, error: `invalid YAML header: ${(e as Error).message}`, warnings: [] };
  }
  const parsed = HeaderSchema.safeParse(raw);
  if (!parsed.success) {
    const issue = parsed.error.issues[0];
    const where = issue?.path.join(".") || "header";
    const why = issue?.message ?? "invalid header";
    return { id, valid: false, error: `${where}: ${why}`, warnings: [] };
  }
  const body = split.body.trim();
  if (!body) {
    return { id, valid: false, error: "recipe has no script body", warnings: [] };
  }
  const directives = parseSbatchDirectives(body);
  return { id, valid: true, header: parsed.data, body, directives, warnings: directives.warnings };
}
