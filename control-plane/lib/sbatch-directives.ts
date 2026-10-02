// Lifts `#SBATCH` directives out of a script body into the structured fields
// obleth sends to slurmrestd. slurmrestd does NOT honor `#SBATCH` comment
// directives (they are an `sbatch`-CLI feature), so a recipe's job parameters
// must be parsed out of the script and sent as JSON. This module is pure
// (no fs / no network, no Node built-ins) and safe to import anywhere,
// including the dashboard's client components.

export interface ParsedDirectives {
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
  log_output_dir?: string;
  chdir?: string;
  warnings: string[];
}

/** Canonical long-form key for a directive token (maps short flags to long). */
const SHORT_TO_LONG: Record<string, string> = {
  "-p": "partition",
  "-c": "cpus-per-task",
  "-N": "nodes",
  "-A": "account",
  "-q": "qos",
  "-C": "constraint",
  "-x": "exclude",
  "-t": "time",
  "-o": "output",
  "-e": "error",
  "-D": "chdir",
};

/** Split one directive (already stripped of `#SBATCH` and comments) into key + value. */
function splitDirective(rest: string): { key: string; value: string } | null {
  const trimmed = rest.trim();
  if (!trimmed.startsWith("-")) return null;
  // long form with `=`
  if (trimmed.startsWith("--") && trimmed.includes("=")) {
    const eq = trimmed.indexOf("=");
    return { key: trimmed.slice(2, eq), value: trimmed.slice(eq + 1).trim() };
  }
  const parts = trimmed.split(/\s+/);
  const flag = parts[0];
  const value = parts.slice(1).join(" ").trim();
  if (flag.startsWith("--")) return { key: flag.slice(2), value };
  const long = SHORT_TO_LONG[flag];
  if (!long) return { key: flag, value }; // unknown short flag -> warned downstream
  return { key: long, value };
}

/** POSIX dirname: "logs/serve-%j.out" → "logs", "/a/b.out" → "/a", "b.out" → ".". */
function dirname(p: string): string {
  const s = p.replace(/\/+$/, "");
  const i = s.lastIndexOf("/");
  if (i === -1) return ".";
  return s.slice(0, i).replace(/\/+$/, "") || "/";
}

/** Remove a trailing ` # comment` from a directive line (values here never contain `#`). */
function stripComment(s: string): string {
  const i = s.search(/\s#/);
  return (i === -1 ? s : s.slice(0, i)).trim();
}

/** A script with Unix line endings. A browser sends a textarea's text with
 *  CRLF, and bash reads the `\r` as part of each line: `#!/bin/bash -l\r`
 *  passes the option `-\r` and the job exits before its first command. */
export function unixLineEndings(script: string): string {
  return script.replace(/\r\n/g, "\n");
}

const isDirective = (line: string) => line.trim().startsWith("#SBATCH");

/** One `#SBATCH` line read into its long-form key and value, or null. */
function readDirective(line: string): { key: string; value: string } | null {
  const t = line.trim();
  if (!t.startsWith("#SBATCH")) return null;
  return splitDirective(stripComment(t.slice("#SBATCH".length)));
}

/** The script without its `#SBATCH` lines. obleth sends job settings as
 *  fields, so in a script that is submitted they are only comments. */
export function stripSbatchDirectives(script: string): string {
  return script.split("\n").filter((line) => !isDirective(line)).join("\n");
}

/** The placement settings a deployment's page edits, by the field they set. */
export type PlacementKey = "partition" | "gres" | "cpus_per_task" | "mem" | "nodes" | "account" | "qos" | "time_limit" | "constraints" | "exclude";

const PLACEMENT_OF: Record<string, PlacementKey> = {
  partition: "partition",
  gres: "gres",
  "cpus-per-task": "cpus_per_task",
  mem: "mem",
  nodes: "nodes",
  account: "account",
  qos: "qos",
  time: "time_limit",
  constraint: "constraints",
  exclude: "exclude",
};

export interface SbatchLine {
  /** The directive as written, without `#SBATCH`, e.g. "--mem=500G". */
  text: string;
  value: string;
  /** The placement setting it would set; null when obleth has none for it. */
  setting: PlacementKey | null;
}

/** Every `#SBATCH` line in a script, with the placement setting each matches. */
export function sbatchLines(script: string): SbatchLine[] {
  const out: SbatchLine[] = [];
  for (const line of script.split("\n")) {
    if (!isDirective(line)) continue;
    const text = stripComment(line.trim().slice("#SBATCH".length));
    const parsed = readDirective(line);
    out.push({ text, value: parsed?.value ?? "", setting: (parsed && PLACEMENT_OF[parsed.key]) ?? null });
  }
  return out;
}

export function parseSbatchDirectives(script: string): ParsedDirectives {
  const out: ParsedDirectives = { warnings: [] };
  for (const line of script.split("\n")) {
    const parsed = readDirective(line);
    if (!parsed) continue;
    const { key, value } = parsed;
    switch (key) {
      case "partition":
        out.partition = value;
        break;
      case "gres":
        out.gres = value;
        break;
      case "cpus-per-task": {
        if (value) {
          const n = Number(value);
          if (Number.isFinite(n)) out.cpus_per_task = n;
        }
        break;
      }
      case "mem":
        out.mem = value;
        break;
      case "time":
        out.time_limit = value;
        break;
      case "nodes": {
        if (value) {
          const n = Number(value);
          if (Number.isFinite(n)) out.nodes = n;
        }
        break;
      }
      case "account":
        out.account = value;
        break;
      case "qos":
        out.qos = value;
        break;
      case "constraint":
        out.constraints = value;
        break;
      case "exclude":
        out.exclude = value;
        break;
      case "output":
      case "error": {
        const dir = dirname(value);
        if (dir && dir !== ".") out.log_output_dir = dir;
        break;
      }
      case "chdir":
        out.chdir = value;
        break;
      default:
        const flagStr = key.startsWith("-") ? key : `--${key}`;
        out.warnings.push(`${flagStr} (not applied)`);
    }
  }
  return out;
}
