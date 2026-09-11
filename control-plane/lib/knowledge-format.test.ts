import { describe, expect, it } from "vitest";
import { collectionStatus, distinctEmbeddingModelCount, injectionSkipReason } from "./knowledge-format";
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

  it("reports indexing over a stale embedder when both are true", () => {
    // The state right after kicking off a reindex: needs_reindex is still true
    // (the collection isn't done yet) and the requeued documents are back to
    // pending/indexing. "indexing" is the more useful status here — it says
    // work is in progress, rather than repeating the same "needs re-index"
    // the operator just acted on.
    expect(collectionStatus(stale, docs(["ready", "pending"]))).toBe("indexing");
    expect(collectionStatus(stale, docs(["indexing"]))).toBe("indexing");
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

describe("injectionSkipReason", () => {
  it("attributes a hit below the active minimum score to the threshold", () => {
    expect(injectionSkipReason({ score: 0.4 }, 0.5)).toBe("below threshold");
  });

  it("attributes a hit at or above the threshold to the token budget", () => {
    expect(injectionSkipReason({ score: 0.5 }, 0.5)).toBe("cut by token budget");
    expect(injectionSkipReason({ score: 0.9 }, 0.5)).toBe("cut by token budget");
  });

  it("falls back to the token budget when the active threshold is unknown", () => {
    expect(injectionSkipReason({ score: 0.1 }, null)).toBe("cut by token budget");
  });
});

describe("distinctEmbeddingModelCount", () => {
  it("is zero for no collections", () => {
    expect(distinctEmbeddingModelCount([])).toBe(0);
  });

  it("counts one for collections sharing an embedder", () => {
    expect(
      distinctEmbeddingModelCount([
        { indexed_embedding_model: "bge-small-en" },
        { indexed_embedding_model: "bge-small-en" },
      ]),
    ).toBe(1);
  });

  it("counts each distinct embedder", () => {
    expect(
      distinctEmbeddingModelCount([
        { indexed_embedding_model: "bge-small-en" },
        { indexed_embedding_model: "e5-large" },
        { indexed_embedding_model: "bge-small-en" },
      ]),
    ).toBe(2);
  });
});
