import { type ResourceValue } from "@/components/slurm-launcher/resource-fields";

// Serialized launcher form state. Persisted opaquely as `launcher_spec` jsonb so
// the dashboard "edit" view can restore the panel exactly, and reused as the
// payload for saved recipes. `backendId` holds the selected RECIPE id (template
// or backend base), which resolves back to a recipe + backend family on load.
export type LauncherSpec = {
  backendId?: string;
  model?: string;
  port?: string;
  recipeValues?: Record<string, string>;
  preamble?: string;
  resources?: Partial<ResourceValue>;
  vramGb?: string;
  nodes?: string;
  replicas?: string;
  healthPath?: string;
  maxJobFailures?: string;
  image?: string;
  logOutputDir?: string;
  account?: string;
  qos?: string;
  timeLimit?: string;
  constraints?: string;
  exclude?: string;
  scriptBody?: string;
};
