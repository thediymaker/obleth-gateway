// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { launchRecipeAction, setDeploymentEnabledAction, setDeploymentReplicasAction, setSlurmSettingsAction } from "@/app/actions";
import { DeploymentsList } from "./deployments-list";
import { ManagedPage } from "./managed-page";
import { NewDeployment } from "./new-deployment";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import type { DeploymentsData } from "@/lib/deployments-data";
import type { ManagedModelSpec, ModelReplica, ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({
  launchRecipeAction: vi.fn(async () => ({ ok: true, name: "glm-own" })),
  saveRecipeFromFormAction: vi.fn(async () => ({ ok: true, id: "r1" })),
  lookupHfModelAction: vi.fn(async () => ({ ok: false, error: "offline" })),
  setDeploymentEnabledAction: vi.fn(async () => ({ ok: true })),
  setDeploymentReplicasAction: vi.fn(async () => ({ ok: true })),
  saveDeploymentSettingsAction: vi.fn(async () => ({ ok: true })),
  saveRecipeFromDeploymentAction: vi.fn(async () => ({ ok: true, id: "r1" })),
  removeDeploymentAction: vi.fn(async () => ({ ok: true })),
  restartReplicaAction: vi.fn(async () => ({ ok: true })),
  clearLostReplicasAction: vi.fn(async () => ({ ok: true })),
  deleteTemplateAction: vi.fn(async () => ({ ok: true })),
  saveTemplateAction: vi.fn(async () => ({ ok: true })),
  setSlurmSettingsAction: vi.fn(async () => ({ ok: true })),
  testSlurmConnectionAction: vi.fn(async () => ({ ok: true })),
}));
const push = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: vi.fn(), push, replace: vi.fn() }), usePathname: () => "/deployments" }));

const model = (over: Partial<ModelRoute>): ModelRoute => ({ id: "m1", model_name: "glm-5.2", model_type: "chat", enabled: true, capacity_source: "endpoints", max_in_flight: 8, aliases: [], ...over }) as unknown as ModelRoute;
const spec = (over: Partial<ManagedModelSpec> = {}): ManagedModelSpec => ({
  model_id: "m1", enabled: true, partition: "gh200", gres: "gpu:1", nodes: 1, constraints: null, exclude: null, account: null, qos: null,
  time_limit: "1-00:00:00", cpus_per_task: 72, mem: "560G", image: "", preamble: "", log_output_dir: "logs", launch_command: "", script_body: "llama-server",
  serving_port: 8000, health_path: "/health", min_replicas: 1, target_replicas: 2, max_job_failures: 3, launcher_spec: { name: "Deploy llama.cpp" }, created_at: "", updated_at: "", ...over,
});
const replica = (over: Partial<ModelReplica>): ModelReplica => ({ id: Math.random().toString(36).slice(2), model_id: "m1", slurm_job_id: "42", nodes: "gh-007", endpoint_id: null, state: "pending", last_message: null, created_at: new Date(Date.now() - 60_000).toISOString(), updated_at: new Date().toISOString(), ...over });

function data(over: Partial<DeploymentsData> = {}): DeploymentsData {
  return {
    now: Date.now(),
    models: [model({}), model({ id: "m2", model_name: "glm-5-3-flash", capacity_source: "kubernetes" }), model({ id: "m3", model_name: "static-proxy" })],
    specs: [spec()],
    replicas: [replica({ state: "healthy" }), replica({ last_message: "PENDING — Resources" })],
    discovery: { enabled: true, interval_secs: 15, namespaces: ["aibrix-system-llm"], default_service: "", replicas: 3, models: [{ model_id: "m2", model_name: "glm-5-3-flash", enabled: true, static_max_in_flight: null, enforced_max_in_flight: 160, cluster_in_flight: 38, in_flight: 38, status: { model_name: "glm-5-3-flash", source: "kubernetes", namespaces: [], namespace: "aibrix-system-llm", service: "glm-5-3-flash", ready_replicas: 0, per_replica_max_in_flight: 16, per_replica_source: "configured", headroom: 1, derived_max_in_flight: 0, effective_max_in_flight: 0, state: "discovered", last_refresh: null, last_success: null, reason: null } }] },
    requests: { "glm-5-3-flash": 16039 },
    slurm: { enabled: true, provisioner_running: true, provisioner_last_seen_secs: 4 } as DeploymentsData["slurm"],
    ...over,
  };
}

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, {
    IS_REACT_ACT_ENVIRONMENT: true,
    // jsdom has no IntersectionObserver; the section list only uses it to highlight.
    IntersectionObserver: class { observe() {} unobserve() {} disconnect() {} },
  });
  vi.mocked(launchRecipeAction).mockClear();
  vi.mocked(setDeploymentEnabledAction).mockClear();
  vi.mocked(setDeploymentReplicasAction).mockClear();
  vi.mocked(setSlurmSettingsAction).mockClear();
  push.mockClear();
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

async function render(node: React.ReactNode) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, enabled: false } } });
  await act(async () => root.render(<QueryClientProvider client={client}>{node}</QueryClientProvider>));
}
const button = (text: string) => {
  const b = [...document.querySelectorAll<HTMLButtonElement>("button")].find((x) => x.textContent?.trim() === text || x.getAttribute("aria-label") === text);
  if (!b) throw new Error(`Missing button: ${text}`);
  return b;
};
async function type(el: HTMLInputElement, value: string) {
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(el, value);
    el.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("the Deployments list", () => {
  it("lists launched and watched models, and filters to the ones that need you", async () => {
    await render(<DeploymentsList initial={data()} recipes={[]} tab="deployments" />);
    const table = host.querySelector('[aria-label="Deployments"][class*="overflow-hidden"]')!;
    expect(table.textContent).toContain("glm-5.2");
    expect(table.textContent).toContain("glm-5-3-flash");
    expect(table.textContent).not.toContain("static-proxy");
    expect(table.textContent).toContain("1 queued: Resources");
    expect(host.textContent).toContain("1 on Kubernetes, 0 replicas ready · 1 on Slurm, 1 starting · 1 needs you");
    await act(async () => button("Needs you 1").click());
    expect(table.textContent).not.toContain("glm-5.2");
    expect(table.textContent).toContain("No replicas ready");
  });
});

describe("the Slurm connection", () => {
  const slurm = {
    enabled: true, slurmrestd_url: "http://slurm:6820", slurmrestd_api_version: "v0.0.40", slurm_user: "obleth", jwt_set: true, jwt_last4: "a1b2",
    node_aliases: [{ host: "gh-007", ip: "10.0.0.7" }], provisioner_running: true, provisioner_last_seen_secs: 4, provisioner_tick_status: "ok",
  } as DeploymentsData["slurm"];

  it("keeps the stored JWT unless a new one is typed, and sends the whole connection", async () => {
    await render(<DeploymentsList initial={data({ slurm })} recipes={[]} tab="deployments" slurmSheet />);
    const sheet = document.querySelector('[role="dialog"][aria-label="Slurm connection"]')!;
    expect(sheet.textContent).toContain("A JWT ending a1b2 is set");
    expect(sheet.textContent).toContain("Provisioner checked in just now");
    await type(sheet.querySelector<HTMLInputElement>('input[name="user"]')!, "svc-obleth");
    await act(async () => { await new Promise((r) => requestAnimationFrame(r)); });
    await act(async () => button("Save changes").click());
    expect(setSlurmSettingsAction).toHaveBeenCalledWith({ enabled: true, slurmrestd_url: "http://slurm:6820", slurmrestd_api_version: "v0.0.40", slurm_user: "svc-obleth", node_aliases: [{ host: "gh-007", ip: "10.0.0.7" }], cluster_defaults: { cache_dir: "", images_dir: "", log_dir: "", setup: "", images: {} } });
  });

  it("saves cluster defaults with the connection, and never sends an untouched token", async () => {
    const withDefaults = { ...slurm, cluster_defaults: { cache_dir: "/scratch/hf", images_dir: "/scratch/images", log_dir: "", setup: "", images: { vllm: "vllm.sif" } }, hf_token_set: true, hf_token_last4: "9f3c" } as DeploymentsData["slurm"];
    await render(<DeploymentsList initial={data({ slurm: withDefaults })} recipes={[]} tab="deployments" slurmSheet />);
    await act(async () => button("Cluster defaults").click());
    const sheet = document.querySelector('[role="dialog"][aria-label="Slurm connection"]')!;
    expect(sheet.textContent).toContain("A token ending 9f3c is set");
    await type(sheet.querySelector<HTMLInputElement>('input[aria-label="Ollama image"]')!, "ollama.sif");
    await act(async () => { await new Promise((r) => requestAnimationFrame(r)); });
    await act(async () => button("Save changes").click());
    const body = vi.mocked(setSlurmSettingsAction).mock.calls[0][0];
    expect(body.cluster_defaults).toEqual({ cache_dir: "/scratch/hf", images_dir: "/scratch/images", log_dir: "", setup: "", images: { vllm: "vllm.sif", ollama: "ollama.sif" } });
    expect(body).not.toHaveProperty("hf_token");
  });

  it("asks before turning Slurm off while jobs run", async () => {
    await render(<DeploymentsList initial={data({ slurm })} recipes={[]} tab="deployments" slurmSheet />);
    const off = document.querySelector<HTMLInputElement>('[role="dialog"] input[name="enabled"]')!;
    await act(async () => off.click());
    await act(async () => { await new Promise((r) => requestAnimationFrame(r)); });
    await act(async () => button("Save changes").click());
    expect(document.body.textContent).toContain("Turn Slurm off?");
    expect(setSlurmSettingsAction).not.toHaveBeenCalled();
    await act(async () => button("Turn off").click());
    expect(setSlurmSettingsAction).toHaveBeenCalledWith(expect.objectContaining({ enabled: false }));
  });
});

describe("a Slurm deployment's page", () => {
  it("scales and pauses through the spec", async () => {
    await render(<ManagedPage modelId="m1" initial={data()} changes={[]} />);
    expect(host.textContent).toContain("Slurm says Resources");
    await act(async () => button("One more replica").click());
    expect(setDeploymentReplicasAction).toHaveBeenCalledWith("m1", 3);
  });

  it("says why it stopped after failed launches", async () => {
    const lost = [1, 2, 3].map(() => replica({ state: "lost", last_message: "FAILED — NonZeroExitCode" }));
    await render(<ManagedPage modelId="m1" initial={data({ replicas: lost })} changes={[]} />);
    const why = host.querySelector('[aria-label="Why it stopped"]')!;
    expect(why.textContent).toContain("The last 3 jobs ended without becoming healthy");
    expect(why.textContent).toContain("FAILED — NonZeroExitCode");
  });
});

describe("a new deployment", () => {
  const preview = {
    apiModelName: "glm-5.2", modelType: "chat", engine: "llamacpp", port: 8000, healthPath: "/health", targetReplicas: 2, maxJobFailures: 3, partition: "gh200", gres: "gpu:1", cpusPerTask: 72, mem: "560G", timeLimit: "1-00:00:00",
    scriptBody: "", rawBody: "export LLAMA_CACHE={{cache}}\nllama-server --ctx-size {{context}} {{fa}}", warnings: [], kind: "model" as const,
    inputs: [
      { name: "cache", label: "Weight cache", type: "path" as const, default: "{{cluster.cache}}", required: true },
      { name: "context", label: "Context length", type: "choice" as const, options: ["131072", "1048576"], default: "131072", required: false },
      { name: "fa", label: "Flash attention", type: "flag" as const, default: "true", adds: "--flash-attn on", required: false },
    ],
  };
  const recipe: RecipeCard = { id: "glm-5.2-multiuser", valid: true, name: "GLM-5.2", engine: "llamacpp", modelType: "chat", apiModelName: "glm-5.2", warnings: [], source: "file", preview };
  const cluster = { cache: "/scratch/hf", images: "", logs: "", setup: "", image: {} };
  const launch = (over: Record<string, unknown>) => ({ id: Math.random().toString(36), model_name: "glm-a", recipe_id: "glm-5.2-multiuser", partition: "gh200", account: "ai-research", qos: null, time_limit: "1-00:00:00", nodes_requested: 1, nodes: "gh-007", submitted_at: new Date(Date.now() - 3_600_000).toISOString(), started_at: null, healthy_at: null, ended_at: null, end_state: null, queued_secs: 120, load_secs: 900, ...over });

  it("starts from the recipe and cluster defaults, then launches under the name you give it", async () => {
    await render(<NewDeployment recipes={[recipe]} takenNames={["glm-5.2"]} slurmOn initialRecipe="glm-5.2-multiuser" cluster={cluster} />);
    await act(async () => button("Choose where it runs →").click());
    // The taken name gets a free variant, and the cache comes from the cluster default.
    expect(host.querySelector<HTMLInputElement>('input[aria-label="API model name"]')!.value).toBe("glm-5.2-2");
    expect(host.querySelector<HTMLInputElement>('input[aria-label="Weight cache"]')!.value).toBe("/scratch/hf");
    expect(host.textContent).toContain("Cluster default");
    await type(host.querySelector<HTMLInputElement>('input[aria-label="API model name"]')!, "glm-own");
    await act(async () => button("1M").click());
    await act(async () => button("Review the script →").click());
    expect(host.textContent).toContain("export LLAMA_CACHE=/scratch/hf");
    expect(host.textContent).toContain("--ctx-size 1048576 --flash-attn on");
    await act(async () => button("Launch 2 replicas").click());
    expect(launchRecipeAction).toHaveBeenCalledWith("glm-5.2-multiuser", expect.objectContaining({ api_model_name: "glm-own", partition: "gh200", inputs: { cache: "/scratch/hf", context: "1048576", fa: "true" }, target_replicas: 2 }));
    expect(push).toHaveBeenCalledWith("/deployments/glm-own");
  });

  it("won't go on while a required value has no cluster default", async () => {
    await render(<NewDeployment recipes={[recipe]} takenNames={[]} slurmOn initialRecipe="glm-5.2-multiuser" />);
    await act(async () => button("Choose where it runs →").click());
    expect(host.textContent).toContain("Weight cache uses a cluster default that isn't set");
    expect(button("Review the script →").disabled).toBe(true);
    await type(host.querySelector<HTMLInputElement>('input[aria-label="Weight cache"]')!, "/scratch/me");
    expect(button("Review the script →").disabled).toBe(false);
  });

  it("fills in what recent launches agreed on, and says so", async () => {
    await render(<NewDeployment recipes={[recipe]} takenNames={[]} slurmOn initialRecipe="glm-5.2-multiuser" cluster={cluster} launches={[launch({}), launch({})]} />);
    await act(async () => button("Choose where it runs →").click());
    expect(host.querySelector<HTMLInputElement>('input[aria-label="Account"]')!.value).toBe("ai-research");
    expect(host.textContent).toContain("The last 2 launches of this recipe used ai-research.");
    expect(host.textContent).toContain("Learned");
  });

  it("edits the same deployment as YAML", async () => {
    await render(<NewDeployment recipes={[recipe]} takenNames={[]} slurmOn initialRecipe="glm-5.2-multiuser" cluster={cluster} />);
    await act(async () => button("Choose where it runs →").click());
    await act(async () => button("YAML").click());
    const area = host.querySelector<HTMLTextAreaElement>('textarea[aria-label="Deployment YAML"]')!;
    expect(area.value).toContain("recipe: glm-5.2-multiuser");
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, "recipe: glm-5.2-multiuser\nname: from-yaml\nslurm:\n  qos: long\n  reservation: x\n");
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
    expect(host.textContent).toContain("reservation isn't a slurm setting");
    await act(async () => {
      Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, "value")!.set!.call(area, "recipe: glm-5.2-multiuser\nname: from-yaml\nslurm:\n  qos: long\n");
      area.dispatchEvent(new Event("input", { bubbles: true }));
    });
    await act(async () => button("Form").click());
    expect(host.querySelector<HTMLInputElement>('input[aria-label="API model name"]')!.value).toBe("from-yaml");
    expect(host.querySelector<HTMLInputElement>('input[aria-label="QoS"]')!.value).toBe("long");
  });
});
