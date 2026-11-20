// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from "vitest";
const api = vi.hoisted(() => ({
  listCollections: vi.fn(),
  getKnowledgeSettings: vi.fn(),
  listModels: vi.fn(),
  dailyStats: vi.fn(),
  getModelCollections: vi.fn(),
  listDocuments: vi.fn(),
}));
vi.mock("@/lib/obleth", () => ({ obleth: api }));
import { loadCollection, loadKnowledge } from "./knowledge-data";

beforeEach(() => {
  vi.clearAllMocks();
  api.listCollections.mockResolvedValue([
    { id: "c1", name: "Docs" },
    { id: "c2", name: "Policies" },
  ]);
  api.getKnowledgeSettings.mockResolvedValue(null);
  api.listModels.mockResolvedValue([{ id: "m1", model_name: "chat-a", model_type: "chat", boons: ["knowledge"] }]);
  api.dailyStats.mockResolvedValue(null);
  api.getModelCollections.mockResolvedValue({ collection_ids: ["c2"] });
  api.listDocuments.mockImplementation(async (id: string) => [{ id: `${id}-doc` }]);
});

describe("knowledge loaders", () => {
  it("read only the named collection's documents for its page", async () => {
    const data = await loadCollection("Policies");
    expect(api.listDocuments.mock.calls).toEqual([["c2"]]);
    expect(data?.collection.id).toBe("c2");
    expect(data?.documents).toEqual([{ id: "c2-doc" }]);
    expect(data?.usedBy).toEqual({ c2: ["chat-a"] });
  });

  it("find a collection by id too, and nothing for an unknown name", async () => {
    expect((await loadCollection("c1"))?.collection.name).toBe("Docs");
    expect(await loadCollection("nope")).toBeNull();
  });

  it("read every collection's documents for the list", async () => {
    const data = await loadKnowledge();
    expect(api.listDocuments.mock.calls.map(([id]) => id).sort()).toEqual(["c1", "c2"]);
    expect(data.documents).toEqual({ c1: [{ id: "c1-doc" }], c2: [{ id: "c2-doc" }] });
  });
});
