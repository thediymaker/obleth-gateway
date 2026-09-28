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
  setDeploymentEnabledAction: vi.fn(async () => ({ ok: true })),
  setDeploymentReplicasAction: vi.fn(async () => ({ ok: true })),
  saveDeploymentSettingsAction: vi.fn(async () => ({ ok: true })),
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
    expect(setSlurmSettingsAction).toHaveBeenCalledWith({ enabled: true, slurmrestd_url: "http://slurm:6820", slurmrestd_api_version: "v0.0.40", slurm_user: "svc-obleth", node_aliases: [{ host: "gh-007", ip: "10.0.0.7" }] });
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
  const recipe: RecipeCard = {
    id: "glm-5.2-multiuser", valid: true, name: "Deploy llama.cpp", engine: "llama.cpp", modelType: "chat", apiModelName: "glm-5.2", warnings: [], source: "file",
    preview: { apiModelName: "glm-5.2", modelType: "chat", engine: "llama.cpp", port: 8000, healthPath: "/health", targetReplicas: 2, maxJobFailures: 3, partition: "gh200", gres: "gpu:1", cpusPerTask: 72, mem: "560G", timeLimit: "1-00:00:00", scriptBody: "", rawBody: "export LLAMA_CACHE={{cache}}\nllama-server", warnings: [], variables: [{ name: "cache", label: "Weight cache directory", required: true }] },
  };

  it("asks for the recipe's values, then launches under the name you give it", async () => {
    await render(<NewDeployment recipes={[recipe]} takenNames={["glm-5.2"]} slurmOn initialRecipe="glm-5.2-multiuser" />);
    expect(button("Fill in Weight cache directory").disabled).toBe(true);
    await type(document.querySelector<HTMLInputElement>('input[name="var_cache"]')!, "/scratch/me/glm");
    await act(async () => button("Choose where it runs →").click());
    await act(async () => button("Review →").click());
    expect(host.textContent).toContain("A model with this name already exists");
    await type(document.querySelector<HTMLInputElement>('input[name="api_model_name"]')!, "glm-own");
    expect(host.textContent).toContain("export LLAMA_CACHE=/scratch/me/glm");
    await act(async () => button("Launch 2 replicas").click());
    expect(launchRecipeAction).toHaveBeenCalledWith("glm-5.2-multiuser", expect.objectContaining({ api_model_name: "glm-own", partition: "gh200", variables: { cache: "/scratch/me/glm" }, target_replicas: 2 }));
    expect(push).toHaveBeenCalledWith("/deployments/glm-own");
  });
});
