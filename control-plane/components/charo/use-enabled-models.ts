// control-plane/components/charo/use-enabled-models.ts
"use client";

import { useEffect, useState } from "react";
import type { ModelRoute } from "@/lib/obleth";

/** Load the enabled model list once (shared by workflow cards that pick a model). */
export function useEnabledModels(): { models: ModelRoute[]; loading: boolean; error: string | null; reload: () => void } {
  const [models, setModels] = useState<ModelRoute[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [revision, setRevision] = useState(0);
  useEffect(() => {
    let cancelled = false;
    setLoading(true); setError(null);
    fetch("/api/live/models")
      .then((r) => { if (!r.ok) throw new Error("model list unavailable"); return r.json(); })
      .then((list: ModelRoute[]) => {
        if (cancelled) return;
        setModels(list.filter((m) => m.enabled));
      })
      .catch(() => { if (!cancelled) setError("Unable to load models from the gateway."); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [revision]);
  return { models, loading, error, reload: () => setRevision((r) => r + 1) };
}
