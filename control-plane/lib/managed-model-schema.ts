// Validation for a Slurm deployment's launch settings (its page's form and
// saveDeploymentSettingsAction). The schema mirrors what the obleth
// admin API (`PUT /models/:id/managed`) and Slurm will accept, so obvious
// mistakes (bad `time_limit` format, out-of-range port, empty partition) are
// caught before submit instead of surfacing as an opaque slurmrestd 500.
//
// Keys match the form field `name`s (`slurm_*`) so a Zod issue path maps
// straight back to the input that produced it.
import { z } from "zod";

/**
 * Slurm `--time` accepts: minutes, `MM:SS`, `HH:MM:SS`, `D-HH`, `D-HH:MM`, or
 * `D-HH:MM:SS`. e.g. `120`, `30:00`, `4:00:00`, `0-04:00:00`.
 */
export const SLURM_TIME_LIMIT_RE =
  /^(?:\d+|\d+:\d{1,2}|\d+:\d{1,2}:\d{1,2}|\d+-\d+(?::\d{1,2}(?::\d{1,2})?)?|UNLIMITED|INFINITE)$/i;

/**
 * `--gres` per node, as Slurm takes it: a comma-separated list of
 * `name[:type][:count]`. Types may hold letters, digits, `_`, `.` and `-`,
 * e.g. `gpu`, `gpu:1`, `gpu:h100:2`, `gpu:nvidia_a100-sxm4-80gb:1`,
 * `gpu:3g.40gb:1`, `gpu:1,shard:1`.
 */
export const GRES_RE = /^[A-Za-z0-9_]+(?::[A-Za-z0-9_.-]+)?(?::\d+)?(?:,[A-Za-z0-9_]+(?::[A-Za-z0-9_.-]+)?(?::\d+)?)*$/;

/** The ports each replica gets, starting at its serving port. */
export const PORT_WINDOW = 8;

/** `--mem` per node: an integer with an optional K/M/G/T unit. e.g. `560G`. */
export const MEM_RE = /^\d+(?:\.\d+)?[KMGT]?B?$/i;

const isBlank = (v: string) => v.trim() === "";

/** A field that may be blank, but if filled must match `re`. */
function optionalMatch(re: RegExp, message: string) {
  return z.string().refine((v) => isBlank(v) || re.test(v.trim()), { message });
}

/** A required whole number within an inclusive range. */
function requiredInt(min: number, max: number, message: string) {
  return z.string().refine((v) => {
    const t = v.trim();
    return /^\d+$/.test(t) && Number(t) >= min && Number(t) <= max;
  }, { message });
}

/** A field that may be blank, but if filled must be a whole number ≥ `min`. */
function optionalInt(min: number, message: string) {
  return z.string().optional().refine((v) => {
    const t = (v ?? "").trim();
    return t === "" || (/^\d+$/.test(t) && Number(t) >= min);
  }, { message });
}

export const managedModelFormSchema = z
  .object({
    // Blank means the cluster's default partition.
    slurm_partition: z.string(),
    slurm_gres: optionalMatch(GRES_RE, "Write GPUs as Slurm gres, such as gpu:1, gpu:h100:2 or gpu:1,shard:1."),
    slurm_cpus_per_task: optionalInt(0, "CPUs must be a whole number, or blank for the cluster default."),
    slurm_mem: optionalMatch(MEM_RE, "Size with an optional unit, e.g. 560G, 32G, or 4096M."),
    slurm_nodes: requiredInt(1, 100_000, "Whole number ≥ 1."),
    slurm_qos: z.string(),
    slurm_account: z.string(),
    slurm_time_limit: optionalMatch(
      SLURM_TIME_LIMIT_RE,
      "Write the walltime as Slurm does: D-HH:MM:SS, HH:MM:SS, MM:SS, minutes, or UNLIMITED.",
    ),
    slurm_constraints: z.string(),
    slurm_exclude: z.string(),
    slurm_serving_port: requiredInt(1, 65_536 - PORT_WINDOW, `The port must be between 1 and ${65_536 - PORT_WINDOW}, so each replica's ${PORT_WINDOW} ports fit.`),
    slurm_health_path: z.string().refine((v) => isBlank(v) || v.trim().startsWith("/"), {
      message: "The health path must start with /, such as /health.",
    }),
    slurm_min_replicas: requiredInt(1, 100_000, "Healthy when must be a whole number, at least 1."),
    slurm_target_replicas: requiredInt(0, 100_000, "Replicas must be a whole number."),
    slurm_max_job_failures: requiredInt(0, 100_000, "Stop after failed launches must be a whole number; 0 never stops."),
    slurm_startup_timeout_mins: optionalInt(0, "Give up loading after must be a whole number of minutes; 0 never gives up."),
    slurm_handoff_mins: optionalInt(0, "Start a replacement must be a whole number of minutes; 0 turns it off."),
    // A relative path would be read from the job's working directory, /tmp on the node.
    slurm_log_output_dir: z.string().optional().refine((v) => v === undefined || isBlank(v) || v.trim().startsWith("/"), {
      message: "Job logs must be a full path starting with /, such as /shared/logs, or blank.",
    }),
  })
  .passthrough()
  .superRefine((v, ctx) => {
    const min = Number(v.slurm_min_replicas);
    const target = Number(v.slurm_target_replicas);
    if (Number.isFinite(min) && Number.isFinite(target) && target > 0 && min > target) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["slurm_min_replicas"],
        message: "Healthy when can't be more than the replica count.",
      });
    }
  });

export type ManagedModelFormValues = z.input<typeof managedModelFormSchema>;

/**
 * Validate the raw form strings. Returns a map of field name → first error
 * message (empty when valid) so the caller can render errors inline.
 */
export function validateManagedModelForm(values: Record<string, string>, stored?: Record<string, string>): Record<string, string> {
  const parsed = managedModelFormSchema.safeParse(values);
  if (parsed.success) return {};
  const errors: Record<string, string> = {};
  for (const issue of parsed.error.issues) {
    const key = String(issue.path[0] ?? "");
    // A value saved before (by a launch, or by an older check) isn't held
    // against an unrelated change; only what changed is checked.
    if (stored && key in stored && (stored[key] ?? "").trim() === (values[key] ?? "").trim()) continue;
    if (key && !errors[key]) errors[key] = issue.message;
  }
  return errors;
}

/** A stored spec as the form's `slurm_*` strings, to tell what a save changed. */
export function specFormValues(s: Partial<{
  partition: string; gres: string; nodes: number; cpus_per_task: number | null; mem: string | null; account: string | null; qos: string | null;
  time_limit: string | null; constraints: string | null; exclude: string | null; serving_port: number; health_path: string; min_replicas: number;
  target_replicas: number; max_job_failures: number; log_output_dir: string | null; startup_timeout_mins: number; handoff_mins: number;
}>): Record<string, string> {
  const str = (v: unknown) => (v == null ? "" : String(v));
  return {
    slurm_partition: str(s.partition), slurm_gres: str(s.gres), slurm_nodes: str(s.nodes), slurm_cpus_per_task: s.cpus_per_task ? str(s.cpus_per_task) : "",
    slurm_mem: str(s.mem), slurm_account: str(s.account), slurm_qos: str(s.qos), slurm_time_limit: str(s.time_limit), slurm_constraints: str(s.constraints),
    slurm_exclude: str(s.exclude), slurm_serving_port: str(s.serving_port), slurm_health_path: str(s.health_path), slurm_min_replicas: str(s.min_replicas),
    slurm_target_replicas: str(s.target_replicas), slurm_max_job_failures: str(s.max_job_failures), slurm_log_output_dir: str(s.log_output_dir),
    slurm_startup_timeout_mins: str(s.startup_timeout_mins), slurm_handoff_mins: str(s.handoff_mins),
  };
}
