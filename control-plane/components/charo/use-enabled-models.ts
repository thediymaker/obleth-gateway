// control-plane/components/charo/use-enabled-models.ts
"use client";

import { useEffect, useState } from "react";
import type { ModelRoute } from "@/lib/obleth";

/** Load the enabled model list once (shared by workflow cards that pick a model). */
export function useEnabledModels(): { models: ModelRoute[]; loading: boolean } {
  const [models, setModels] = useState<ModelRoute[]>([]);
  const [loading, setLoading] = useState(true);
  useEffect(() => {
    let cancelled = false;
    fetch("/api/live/models")
      .then((r) => (r.ok ? r.json() : []))
      .then((list: ModelRoute[]) => {
        if (cancelled) return;
        setModels(list.filter((m) => m.enabled));
      })
      .catch(() => {})
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, []);
  return { models, loading };
}
