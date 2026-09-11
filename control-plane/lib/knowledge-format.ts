// Domain-specific formatting for the knowledge (RAG) dashboard. Generic byte
// formatting lives in `@/lib/format` (`formatBytes`) since it isn't specific
// to this feature; this module holds the collection status rollup plus the
// small pieces of retrieval-preview and model-attachment reasoning that are
// worth unit-testing on their own.
import type { KnowledgeCollection, KnowledgeDocument, KnowledgeHit } from "@/lib/obleth";

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

/**
 * Explains why a retrieval-preview hit that scored well still won't be
 * injected. `POST /knowledge/collections/:id/search` filters to
 * `score >= min_score` before it ever builds the hit list (see
 * `score_against` in `obleth-store`), so a hit that comes back at all has
 * already cleared the threshold — `would_inject: false` on a returned hit can
 * only mean the token budget ran out first. The score comparison here is
 * defensive (covers a future change to that filtering), not the expected
 * path.
 */
export function injectionSkipReason(
  hit: Pick<KnowledgeHit, "score">,
  minScore: number | null,
): "below threshold" | "cut by token budget" {
  if (minScore != null && hit.score < minScore) return "below threshold";
  return "cut by token budget";
}

/**
 * Distinct `indexed_embedding_model` values across a set of collections. Used
 * to warn that attaching collections indexed with different embedders adds a
 * network round trip per embedder at request time. Reads
 * `indexed_embedding_model` (the embedder the active generation was actually
 * built with), not `embedding_model` (the operator's currently desired one —
 * they differ only mid-reindex).
 */
export function distinctEmbeddingModelCount(
  collections: Pick<KnowledgeCollection, "indexed_embedding_model">[],
): number {
  return new Set(collections.map((c) => c.indexed_embedding_model)).size;
}
