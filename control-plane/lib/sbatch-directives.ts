// Lifts `#SBATCH` directives out of a script body into the structured fields
// obleth sends to slurmrestd. slurmrestd does NOT honor `#SBATCH` comment
// directives (they are an `sbatch`-CLI feature), so a recipe's job parameters
// must be parsed out of the script and sent as JSON. This module is pure
// (no fs / no network) and safe to import anywhere.
import path from "node:path";

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

/** Remove a trailing ` # comment` from a directive line (values here never contain `#`). */
function stripComment(s: string): string {
  const i = s.search(/\s#/);
  return (i === -1 ? s : s.slice(0, i)).trim();
}

export function parseSbatchDirectives(script: string): ParsedDirectives {
  const out: ParsedDirectives = { warnings: [] };
  for (const line of script.split("\n")) {
    const t = line.trim();
    if (!t.startsWith("#SBATCH")) continue;
    const rest = stripComment(t.slice("#SBATCH".length));
    const parsed = splitDirective(rest);
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
        const dir = path.posix.dirname(value);
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
