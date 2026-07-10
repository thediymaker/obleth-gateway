"use client";

import { cn } from "@/lib/utils";
import type { Activity } from "@/lib/charo/activities/types";
import { ensureActivitiesRegistered, listActivities } from "@/lib/charo/activities";

function useActivities(): Activity[] {
  ensureActivitiesRegistered();
  return listActivities();
}

const TINT: Record<string, string> = {
  test_capabilities: "bg-violet-500/15 text-violet-300",
  chat_with_model: "bg-sky-400/15 text-sky-300",
  benchmark: "bg-emerald-400/15 text-emerald-300",
};
const DEFAULT_TINT = "bg-violet-500/15 text-violet-300";

export function ActivityCards({ onPick }: { onPick: (a: Activity) => void }) {
  const activities = useActivities();
  return (
    <div className="w-full space-y-0.5">
      {activities.map((a) => {
        const Icon = a.icon;
        return (
          <button
            key={a.id}
            type="button"
            onClick={() => onPick(a)}
            className="flex w-full items-center gap-3 rounded-[10px] px-2.5 py-[9px] text-left transition-colors hover:bg-violet-500/[0.07]"
          >
            <span className={cn("flex h-[30px] w-[30px] shrink-0 items-center justify-center rounded-lg", TINT[a.id] ?? DEFAULT_TINT)}>
              {Icon && <Icon className="h-[15px] w-[15px]" />}
            </span>
            <span className="min-w-0">
              <span className="block text-[13px] font-semibold text-foreground">{a.label}</span>
              <span className="block text-[11.5px] leading-snug text-muted-foreground">{a.blurb}</span>
            </span>
          </button>
        );
      })}
    </div>
  );
}
