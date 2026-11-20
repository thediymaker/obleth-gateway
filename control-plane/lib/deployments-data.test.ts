import { beforeEach, describe, expect, it, vi } from "vitest";
import { obleth, type ModelRoute } from "@/lib/obleth";
import { loadDeployment, loadDeploymentById, loadDeploymentRequests, loadDeployments } from "./deployments-data";

vi.mock("@/lib/obleth", () => ({
  obleth: {
    listModels: vi.fn(),
    getModel: vi.fn(),
    listManagedModels: vi.fn(),
    getManagedModel: vi.fn(),
    listAllReplicas: vi.fn(),
    listReplicas: vi.fn(),
    capacityDiscovery: vi.fn(),
    getSlurmSettings: vi.fn(),
    usageByModel: vi.fn(),
  },
}));
vi.spyOn(console, "error").mockImplementation(() => {});

const client = vi.mocked(obleth);
const slurmModel = { id: "m1", model_name: "glm", capacity_source: "endpoints" } as ModelRoute;
const watchedModel = { id: "m2", model_name: "flash", capacity_source: "kubernetes" } as ModelRoute;

beforeEach(() => {
  vi.clearAllMocks();
  client.listModels.mockResolvedValue([slurmModel, watchedModel] as never);
  client.getModel.mockResolvedValue(slurmModel as never);
  client.listManagedModels.mockResolvedValue([] as never);
  client.getManagedModel.mockResolvedValue({ model_id: "m1" } as never);
  client.listAllReplicas.mockResolvedValue([] as never);
  client.listReplicas.mockResolvedValue([{ id: "r1", model_id: "m1" }] as never);
  client.capacityDiscovery.mockResolvedValue({ models: [] } as never);
  client.getSlurmSettings.mockResolvedValue({ enabled: true } as never);
  client.usageByModel.mockResolvedValue([{ model: "glm", requests: "40" }] as never);
});

const fleetReads = () => [client.listModels, client.listManagedModels, client.listAllReplicas, client.usageByModel];

describe("a deployment's own loader", () => {
  it("reads only the per-model endpoints for a Slurm deployment", async () => {
    const d = await loadDeployment(slurmModel);
    expect(client.getManagedModel).toHaveBeenCalledWith("m1");
    expect(client.listReplicas).toHaveBeenCalledWith("m1");
    for (const f of fleetReads()) expect(f).not.toHaveBeenCalled();
    expect(client.capacityDiscovery).not.toHaveBeenCalled();
    expect(d).toMatchObject({ model: slurmModel, spec: { model_id: "m1" }, replicas: [{ id: "r1" }], discovery: null, slurm: { enabled: true }, failed: [] });
  });

  it("reads capacity discovery for a watched model", async () => {
    client.getManagedModel.mockResolvedValue(null as never);
    const d = await loadDeployment(watchedModel);
    expect(client.capacityDiscovery).toHaveBeenCalledOnce();
    for (const f of fleetReads()) expect(f).not.toHaveBeenCalled();
    expect(d.spec).toBeNull();
    expect(d.discovery).toEqual({ models: [] });
  });

  it("says which reads failed", async () => {
    client.listReplicas.mockRejectedValue(new Error("down"));
    const d = await loadDeployment(slurmModel);
    expect(d.failed).toEqual(["replicas"]);
    expect(d.replicas).toEqual([]);
  });

  it("looks the model up by id for the live poll", async () => {
    await loadDeploymentById("m1");
    expect(client.getModel).toHaveBeenCalledWith("m1");
    expect(client.listModels).not.toHaveBeenCalled();
  });
});

describe("the Deployments list loaders", () => {
  it("reads the fleet without the 24 h usage aggregation", async () => {
    const d = await loadDeployments();
    expect(client.usageByModel).not.toHaveBeenCalled();
    expect(d).not.toHaveProperty("requests");
  });

  it("reads the 24 h request counts on their own", async () => {
    const now = 1_700_000_000_000;
    expect(await loadDeploymentRequests(now)).toEqual({ glm: 40 });
    expect(client.usageByModel).toHaveBeenCalledWith({ sinceMs: now - 86_400_000 });
    expect(client.listModels).not.toHaveBeenCalled();
  });
});
