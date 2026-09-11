// Domain-specific formatting for the knowledge (RAG) dashboard. Generic byte
// formatting lives in `@/lib/format` (`formatBytes`) since it isn't specific
// to this feature; this module only holds the collection status rollup.
import type { KnowledgeCollection, KnowledgeDocument } from "@/lib/obleth";

/**
 * Roll a collection's own `needs_reindex` flag together with its documents'
 * indexing states into one status an operator can scan at a glance.
 *
 * Failures come first because they need attention; a stale embedder outranks
 * "ready" because retrieval is silently running on the old vectors even
 * though every document individually looks fine.
 */
export function collectionStatus(
  collection: KnowledgeCollection,
  documents: KnowledgeDocument[],
): "ready" | "indexing" | "needs re-index" | "failed" {
  if (documents.some((d) => d.status === "failed")) return "failed";
  if (documents.some((d) => d.status === "pending" || d.status === "indexing")) {
    return "indexing";
  }
  if (collection.needs_reindex) return "needs re-index";
  return "ready";
}
