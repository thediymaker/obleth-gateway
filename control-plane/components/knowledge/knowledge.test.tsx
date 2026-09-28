// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { attachCollectionAction, setKnowledgeEnabledAction } from "@/app/actions";
import { CollectionPage } from "./collection-page";
import { KnowledgeList } from "./knowledge-list";
import type { KnowledgeData } from "@/lib/knowledge-data";
import type { KnowledgeCollection, KnowledgeDocument, KnowledgeSettingsView, ModelRoute } from "@/lib/obleth";

vi.mock("@/app/actions", () => ({
  setKnowledgeEnabledAction: vi.fn(async () => ({ ok: true })),
  saveRetrievalSettingsAction: vi.fn(async () => ({ ok: true })),
  attachCollectionAction: vi.fn(async () => ({ ok: true })),
}));
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: vi.fn(), push: vi.fn(), replace: vi.fn() }), usePathname: () => "/knowledge" }));

const collection: KnowledgeCollection = { id: "c1", name: "Supercomputing Data", description: "RC Supercomputing", embedding_model: "qwen3-embedding-4b", indexed_embedding_model: "qwen3-embedding-4b", embedding_dim: 2560, chunk_tokens: 400, chunk_overlap_tokens: 50, needs_reindex: false, chunk_count: 32, estimated_bytes: 371077 };
const doc = (title: string, over: Partial<KnowledgeDocument> = {}): KnowledgeDocument => ({ id: title, collection_id: "c1", title, filename: title, content_type: "text/markdown", byte_size: 6405, status: "ready", error: null, chunk_count: 5, reindex_requested: false, ...over });
const settings = { enabled: false, top_k: 5, min_score: 0.35, max_context_tokens: 1500, embed_timeout_ms: 1500, query_cache_ttl_s: 600, query_turns: 2, max_upload_bytes: 10485760, max_chunks_per_collection: 100000, index_batch_size: 32, index_timeout_ms: 30000, index_stale_after_secs: 1800, debug_snapshot: false } as KnowledgeSettingsView;
const models = [{ id: "m1", model_name: "qwen38-27b", model_type: "chat", boons: [] }, { id: "e1", model_name: "qwen3-embedding-4b", model_type: "embedding" }] as unknown as ModelRoute[];

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
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
const button = (text: string) => [...document.querySelectorAll<HTMLButtonElement>("button")].find((b) => b.textContent?.trim() === text)!;

describe("knowledge", () => {
  it("leads with retrieval being off, and turns it on", async () => {
    const data: KnowledgeData = { collections: [collection], documents: { c1: [doc("slurm-fairshare-score.md"), doc("broken.md", { status: "failed", error: "not UTF-8" })] }, settings, models, usedBy: {}, stats: null };
    await render(<KnowledgeList data={data} tab="collections" />);
    expect(host.querySelector('[aria-label="Retrieval is off"]')).toBeTruthy();
    const row = host.querySelector('[aria-label="Collections"] a')!;
    expect(row.getAttribute("href")).toBe("/knowledge/Supercomputing%20Data");
    expect(row.textContent).toContain("Failed");
    await act(async () => button("Turn retrieval on").click());
    expect(setKnowledgeEnabledAction).toHaveBeenCalledWith(true);
  });

  it("lists a collection's documents, failures first, and attaches it to a model", async () => {
    await render(<CollectionPage collection={collection} initialDocuments={[doc("a.md"), doc("b.md", { status: "failed", error: "not UTF-8" })]} settings={settings} models={models} usedBy={[]} stats={null} changes={[]} />);
    const docs = host.querySelector('[aria-label="Documents"]')!;
    expect(docs.textContent!.indexOf("b.md")).toBeLessThan(docs.textContent!.indexOf("a.md"));
    expect(docs.textContent).toContain("not UTF-8");
    expect(host.querySelector('input[type="file"]')!.getAttribute("accept")).toContain(".md");
    // Open the model picker from the keyboard, pick the model, then attach.
    const trigger = host.querySelector<HTMLButtonElement>('[aria-label="Model to attach"]')!;
    await act(async () => { trigger.focus(); trigger.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })); });
    const option = [...document.querySelectorAll<HTMLElement>('[role="menuitem"]')].find((o) => o.textContent?.includes("qwen38-27b"))!;
    await act(async () => option.click());
    await act(async () => button("Attach").click());
    expect(attachCollectionAction).toHaveBeenCalledWith("m1", "c1", true);
  });
});
