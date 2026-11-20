import { obleth, type DailyStatsView, type KnowledgeCollection, type KnowledgeDocument, type KnowledgeSettingsView, type ModelRoute } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export interface KnowledgeData {
  collections: KnowledgeCollection[];
  documents: Record<string, KnowledgeDocument[]>;
  settings: KnowledgeSettingsView | null;
  models: ModelRoute[];
  /** Collection id → names of models that retrieve from it (Knowledge boon on and attached). */
  usedBy: Record<string, string[]>;
  stats: DailyStatsView | null;
}

/** What every Knowledge page reads: everything but documents. */
async function loadShared(): Promise<Omit<KnowledgeData, "documents">> {
  const [collections, settings, models, stats] = await Promise.all([
    safe(obleth.listCollections(), [] as KnowledgeCollection[]),
    safe<KnowledgeSettingsView | null>(obleth.getKnowledgeSettings(), null),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe<DailyStatsView | null>(obleth.dailyStats("knowledge", 30), null),
  ]);
  // Only models with the Knowledge boon retrieve, so only theirs are read.
  const retrievers = models.filter((m) => m.model_type === "chat" && (m.boons ?? []).includes("knowledge"));
  const links = await Promise.all(
    retrievers.map((m) => safe(obleth.getModelCollections(m.id).then((r) => r.collection_ids), [] as string[])),
  );
  const usedBy: Record<string, string[]> = {};
  retrievers.forEach((m, i) => {
    for (const id of links[i]) (usedBy[id] ??= []).push(m.model_name);
  });
  return { collections, settings, models, usedBy, stats };
}

/** The Knowledge list: every collection with its documents. */
export async function loadKnowledge(): Promise<KnowledgeData> {
  const shared = await loadShared();
  const docs = await Promise.all(
    shared.collections.map((c) => safe(obleth.listDocuments(c.id), [] as KnowledgeDocument[])),
  );
  return { ...shared, documents: Object.fromEntries(shared.collections.map((c, i) => [c.id, docs[i]])) };
}

/**
 * One collection's page, found by name (or id), with only its own documents;
 * `null` when there is no such collection.
 */
export async function loadCollection(
  nameOrId: string,
): Promise<(Omit<KnowledgeData, "documents"> & { collection: KnowledgeCollection; documents: KnowledgeDocument[] }) | null> {
  const shared = await loadShared();
  const collection =
    shared.collections.find((c) => c.name === nameOrId) ?? shared.collections.find((c) => c.id === nameOrId);
  if (!collection) return null;
  const documents = await safe(obleth.listDocuments(collection.id), [] as KnowledgeDocument[]);
  return { ...shared, collection, documents };
}
