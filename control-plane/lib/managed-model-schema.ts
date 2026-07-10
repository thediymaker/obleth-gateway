// Client-side validation + inline help for the Provisioning settings form
// (components/managed-model-config.tsx). The schema mirrors what the obleth
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
  /^(?:\d+|\d+:\d{1,2}|\d+:\d{1,2}:\d{1,2}|\d+-\d+(?::\d{1,2}(?::\d{1,2})?)?)$/;

/** `--gres` per node: `name[:type]:count`. e.g. `gpu:1`, `gpu:h100:2`. */
export const GRES_RE = /^[A-Za-z0-9_]+(?::[A-Za-z0-9_]+)*:\d+$/;

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
  return z.string().refine((v) => {
    const t = v.trim();
    return t === "" || (/^\d+$/.test(t) && Number(t) >= min);
  }, { message });
}

export const managedModelFormSchema = z
  .object({
    slurm_partition: z.string().refine((v) => !isBlank(v), {
      message: "Partition is required.",
    }),
    slurm_gres: optionalMatch(GRES_RE, "Use name:count, e.g. gpu:1 or gpu:h100:2."),
    slurm_cpus_per_task: optionalInt(1, "Whole number ≥ 1, or blank for the cluster default."),
    slurm_mem: optionalMatch(MEM_RE, "Size with an optional unit, e.g. 560G, 32G, or 4096M."),
    slurm_nodes: requiredInt(1, 100_000, "Whole number ≥ 1."),
    slurm_qos: z.string(),
    slurm_account: z.string(),
    slurm_time_limit: optionalMatch(
      SLURM_TIME_LIMIT_RE,
      "Slurm walltime: D-HH:MM:SS, HH:MM:SS, MM:SS, or minutes.",
    ),
    slurm_constraints: z.string(),
    slurm_exclude: z.string(),
    slurm_serving_port: requiredInt(1, 65_535, "Port between 1 and 65535."),
    slurm_health_path: z.string().refine((v) => isBlank(v) || v.trim().startsWith("/"), {
      message: "Must start with / (e.g. /health).",
    }),
    slurm_min_replicas: requiredInt(0, 100_000, "Whole number ≥ 0."),
    slurm_target_replicas: requiredInt(1, 100_000, "Whole number ≥ 1."),
    slurm_max_job_failures: requiredInt(0, 100_000, "Whole number ≥ 0 (0 = no limit)."),
  })
  .passthrough()
  .superRefine((v, ctx) => {
    const min = Number(v.slurm_min_replicas);
    const target = Number(v.slurm_target_replicas);
    if (Number.isFinite(min) && Number.isFinite(target) && min > target) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["slurm_min_replicas"],
        message: "Min replicas can't exceed target replicas.",
      });
    }
  });

export type ManagedModelFormValues = z.input<typeof managedModelFormSchema>;

/**
 * Validate the raw form strings. Returns a map of field name → first error
 * message (empty when valid) so the caller can render errors inline.
 */
export function validateManagedModelForm(values: Record<string, string>): Record<string, string> {
  const parsed = managedModelFormSchema.safeParse(values);
  if (parsed.success) return {};
  const errors: Record<string, string> = {};
  for (const issue of parsed.error.issues) {
    const key = String(issue.path[0] ?? "");
    if (key && !errors[key]) errors[key] = issue.message;
  }
  return errors;
}

/** Inline help shown under each field label, keyed by form field name. */
export const MANAGED_FIELD_HINTS: Record<string, string> = {
  slurm_partition: "Slurm partition to submit to, e.g. gpu or arm.",
  slurm_gres: "Generic resources per node, e.g. gpu:1 or gpu:h100:2.",
  slurm_cpus_per_task: "--cpus-per-task. Blank uses the cluster default.",
  slurm_mem: "--mem per node, e.g. 560G. Blank uses the cluster default.",
  slurm_nodes: "Nodes per replica job. Usually 1.",
  slurm_qos: "Quality-of-service name. Blank uses the partition default.",
  slurm_account: "Slurm account to charge. Blank uses your default association.",
  slurm_time_limit: "Walltime as D-HH:MM:SS, HH:MM:SS, or minutes, e.g. 0-04:00:00.",
  slurm_constraints: "--constraint node feature expression, e.g. h200&nvlink.",
  slurm_exclude: "Nodes to keep off, e.g. node[01-04].",
  slurm_serving_port: "Port your server listens on inside the job, e.g. 8000.",
  slurm_health_path: "HTTP path probed for readiness, e.g. /health.",
  slurm_min_replicas: "Healthy replicas required before the model serves traffic.",
  slurm_target_replicas: "Replicas the provisioner keeps running.",
  slurm_max_job_failures: "Stop resubmitting after this many failed launches. 0 = no limit.",
};
