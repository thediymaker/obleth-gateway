"use client";

import { useCallback, useState } from "react";
import { saveRetrievalSettingsAction } from "@/app/actions";
import { SettingsForm } from "@/components/access/settings-form";
import { SettingsCard } from "@/components/access/ui";
import { Setting, Switch, TextField } from "@/components/models/fields";
import type { KnowledgeSettingsView } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const PROFILES = [
  { key: "standard", label: "Standard", blurb: "5 chunks, score 0.35 or more, up to 1,500 tokens, from the last 2 turns. Right for most collections.", values: { top_k: 5, min_score: 0.35, max_context_tokens: 1500, query_turns: 2 } },
  { key: "precise", label: "Precise", blurb: "3 chunks, score 0.5 or more, up to 800 tokens, from the last turn. When wrong context is worse than none.", values: { top_k: 3, min_score: 0.5, max_context_tokens: 800, query_turns: 1 } },
  { key: "broad", label: "Broad", blurb: "10 chunks, score 0.25 or more, up to 4,000 tokens, from the last 4 turns. For exploratory questions over big collections.", values: { top_k: 10, min_score: 0.25, max_context_tokens: 4000, query_turns: 4 } },
] as const;

type Tuning = { top_k: string; min_score: string; max_context_tokens: string; query_turns: string };

const LABELS: Record<string, string> = {
  enabled: "Retrieval",
  top_k: "Chunks per question",
  min_score: "Minimum score",
  max_context_tokens: "Token budget",
  query_turns: "Question from",
  embed_timeout_ms: "Embedding timeout",
  query_cache_ttl_s: "Query cache",
  max_upload_mb: "Largest upload",
  max_chunks_per_collection: "Chunks per collection",
  index_batch_size: "Index batch",
  index_timeout_ms: "Index timeout",
  index_stale_after_secs: "Stale after",
  debug_snapshot: "Keep chunk text in traces",
};

/** Knowledge retrieval for every model, as one form: on or off, how much it retrieves, and the indexer's limits. */
export function RetrievalSettings({ settings, onSaved }: { settings: KnowledgeSettingsView; onSaved?: () => void }) {
  const [tuning, setTuning] = useState<Tuning>({
    top_k: String(settings.top_k),
    min_score: String(settings.min_score),
    max_context_tokens: String(settings.max_context_tokens),
    query_turns: String(settings.query_turns),
  });
  const active = PROFILES.find((p) => Object.entries(p.values).every(([k, v]) => Number(tuning[k as keyof Tuning]) === v))?.key ?? "custom";
  const set = (k: keyof Tuning) => (e: React.ChangeEvent<HTMLInputElement>) => setTuning((t) => ({ ...t, [k]: e.target.value }));
  const sectionOf = useCallback((n: string) => (n in LABELS ? "retrieval" : null), []);
  const labelOf = useCallback((n: string) => LABELS[n] ?? n, []);

  return (
    <SettingsForm id="knowledge" sectionOf={sectionOf} labelOf={labelOf} save={saveRetrievalSettingsAction} onSaved={() => onSaved?.()} className="flex flex-col gap-4" ariaLabel="Retrieval settings">
      <SettingsCard id="retrieval" title="Retrieval" description="What a model with the Knowledge boon and a collection is handed with each question.">
        <Setting id="set-enabled" label="Retrieval" hint="Off, no model retrieves, whatever it's attached to." fields={["enabled"]} was={{ field: "enabled", checkbox: true }}>
          <Switch name="enabled" label="Retrieval on" defaultChecked={settings.enabled} />
        </Setting>
        <Setting id="set-profile" label="Profile" hint="Sets the four values below; change any of them after." fields={["top_k", "min_score", "max_context_tokens", "query_turns"]}>
          <div className="grid gap-2 lg:grid-cols-3">
            {PROFILES.map((p) => (
              <button
                key={p.key}
                type="button"
                aria-pressed={active === p.key}
                onClick={() => setTuning(Object.fromEntries(Object.entries(p.values).map(([k, v]) => [k, String(v)])) as Tuning)}
                className={cn("flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left", active === p.key ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}
              >
                <span className="text-[12.5px] font-medium">{p.label}</span>
                <span className="text-[11.5px] leading-snug text-muted-foreground">{p.blurb}</span>
              </button>
            ))}
          </div>
          {active === "custom" && <span className="text-xs text-muted-foreground">Custom values.</span>}
        </Setting>
        <Setting id="set-top-k" label="Chunks per question" hint="The most chunks handed to the model." fields={["top_k"]} was={{ field: "top_k" }}>
          <TextField name="top_k" label="Chunks per question" inputMode="numeric" value={tuning.top_k} onChange={set("top_k")} mono className="w-24" />
        </Setting>
        <Setting id="set-min-score" label="Minimum score" hint="0 to 1. Chunks scoring less are left out." fields={["min_score"]} was={{ field: "min_score" }}>
          <TextField name="min_score" label="Minimum score" inputMode="decimal" value={tuning.min_score} onChange={set("min_score")} mono className="w-24" />
        </Setting>
        <Setting id="set-budget" label="Token budget" hint="The most tokens of chunks added to one request." fields={["max_context_tokens"]} was={{ field: "max_context_tokens" }}>
          <TextField name="max_context_tokens" label="Token budget" inputMode="numeric" value={tuning.max_context_tokens} onChange={set("max_context_tokens")} mono className="w-28" />
        </Setting>
        <Setting id="set-turns" label="Question from" hint="How many recent user turns make up the search." fields={["query_turns"]} was={{ field: "query_turns" }}>
          <div className="flex items-center gap-2">
            <TextField name="query_turns" label="Question from" inputMode="numeric" value={tuning.query_turns} onChange={set("query_turns")} mono className="w-20" />
            <span className="text-xs text-muted-foreground">turns</span>
          </div>
        </Setting>
      </SettingsCard>

      <SettingsCard id="indexing" title="Indexing and limits" description="How documents are embedded and what the indexer accepts.">
        <Setting label="Largest upload" hint="Per document." fields={["max_upload_mb"]} was={{ field: "max_upload_mb" }}>
          <div className="flex items-center gap-2"><TextField name="max_upload_mb" label="Largest upload" inputMode="decimal" defaultValue={+(settings.max_upload_bytes / 1024 / 1024).toFixed(2)} mono className="w-24" /><span className="text-xs text-muted-foreground">MB</span></div>
        </Setting>
        <Setting label="Chunks per collection" hint="Uploads that would go over it are refused." fields={["max_chunks_per_collection"]} was={{ field: "max_chunks_per_collection" }}>
          <TextField name="max_chunks_per_collection" label="Chunks per collection" inputMode="numeric" defaultValue={settings.max_chunks_per_collection} mono className="w-32" />
        </Setting>
        <Setting label="Embedding" hint="Chunks per embedding call, and how long one index call may take." fields={["index_batch_size", "index_timeout_ms"]}>
          <div className="flex flex-wrap items-center gap-2">
            <TextField name="index_batch_size" label="Index batch" inputMode="numeric" defaultValue={settings.index_batch_size} mono className="w-20" /><span className="text-xs text-muted-foreground">chunks a call,</span>
            <TextField name="index_timeout_ms" label="Index timeout" inputMode="numeric" defaultValue={settings.index_timeout_ms} mono className="w-24" /><span className="text-xs text-muted-foreground">ms each</span>
          </div>
        </Setting>
        <Setting label="Stuck documents" hint="A document still indexing after this long is retried." fields={["index_stale_after_secs"]} was={{ field: "index_stale_after_secs" }}>
          <div className="flex items-center gap-2"><TextField name="index_stale_after_secs" label="Stale after" inputMode="numeric" defaultValue={settings.index_stale_after_secs} mono className="w-24" /><span className="text-xs text-muted-foreground">seconds</span></div>
        </Setting>
        <Setting label="At request time" hint="How long a question's embedding may take, and how long its results are reused." fields={["embed_timeout_ms", "query_cache_ttl_s"]}>
          <div className="flex flex-wrap items-center gap-2">
            <TextField name="embed_timeout_ms" label="Embedding timeout" inputMode="numeric" defaultValue={settings.embed_timeout_ms} mono className="w-24" /><span className="text-xs text-muted-foreground">ms,</span>
            <TextField name="query_cache_ttl_s" label="Query cache" inputMode="numeric" defaultValue={settings.query_cache_ttl_s} mono className="w-24" /><span className="text-xs text-muted-foreground">s cache</span>
          </div>
        </Setting>
        <Setting label="Keep chunk text in traces" hint="Traces always record which chunks were used; this adds their text, at about 20× the storage." fields={["debug_snapshot"]} was={{ field: "debug_snapshot", checkbox: true }}>
          <Switch name="debug_snapshot" label="Keep chunk text in traces" defaultChecked={settings.debug_snapshot} />
        </Setting>
      </SettingsCard>
    </SettingsForm>
  );
}
