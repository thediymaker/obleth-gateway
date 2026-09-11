import { describe, expect, it } from "vitest";
import { collectionStatus } from "./knowledge-format";
import type { KnowledgeCollection, KnowledgeDocument } from "@/lib/obleth";

const base = { needs_reindex: false } as unknown as KnowledgeCollection;
const stale = { needs_reindex: true } as unknown as KnowledgeCollection;

function docs(statuses: KnowledgeDocument["status"][]): KnowledgeDocument[] {
  return statuses.map((status) => ({ status }) as unknown as KnowledgeDocument);
}

describe("collectionStatus", () => {
  it("reports failure when any document failed", () => {
    expect(collectionStatus(base, docs(["ready", "failed"]))).toBe("failed");
  });

  it("reports failure even when a stale embedder is also present", () => {
    // Failures need attention first, ahead of the (less urgent) stale-embedder case.
    expect(collectionStatus(stale, docs(["ready", "failed"]))).toBe("failed");
  });

  it("reports indexing while work is outstanding", () => {
    expect(collectionStatus(base, docs(["ready", "pending"]))).toBe("indexing");
    expect(collectionStatus(base, docs(["ready", "indexing"]))).toBe("indexing");
  });

  it("surfaces a stale embedder over a clean ready state", () => {
    expect(collectionStatus(stale, docs(["ready"]))).toBe("needs re-index");
  });

  it("reports ready when everything is indexed and current", () => {
    expect(collectionStatus(base, docs(["ready"]))).toBe("ready");
  });

  it("reports ready for an empty collection with a current embedder", () => {
    expect(collectionStatus(base, [])).toBe("ready");
  });

  it("reports needs re-index for an empty collection with a stale embedder", () => {
    expect(collectionStatus(stale, [])).toBe("needs re-index");
  });
});
