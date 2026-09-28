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

/** Everything the Knowledge pages read. */
export async function loadKnowledge(): Promise<KnowledgeData> {
  const [collections, settings, models, stats] = await Promise.all([
    safe(obleth.listCollections(), [] as KnowledgeCollection[]),
    safe<KnowledgeSettingsView | null>(obleth.getKnowledgeSettings(), null),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe<DailyStatsView | null>(obleth.dailyStats("knowledge", 30), null),
  ]);
  // Only models with the Knowledge boon retrieve, so only theirs are read.
  const retrievers = models.filter((m) => m.model_type === "chat" && (m.boons ?? []).includes("knowledge"));
  const [docs, links] = await Promise.all([
    Promise.all(collections.map((c) => safe(obleth.listDocuments(c.id), [] as KnowledgeDocument[]))),
    Promise.all(retrievers.map((m) => safe(obleth.getModelCollections(m.id).then((r) => r.collection_ids), [] as string[]))),
  ]);
  const usedBy: Record<string, string[]> = {};
  retrievers.forEach((m, i) => {
    for (const id of links[i]) (usedBy[id] ??= []).push(m.model_name);
  });
  return {
    collections,
    documents: Object.fromEntries(collections.map((c, i) => [c.id, docs[i]])),
    settings,
    models,
    usedBy,
    stats,
  };
}
