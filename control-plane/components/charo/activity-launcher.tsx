"use client";

import type { Activity } from "@/lib/charo/activities/types";
import { ensureActivitiesRegistered, listActivities } from "@/lib/charo/activities";

function useActivities(): Activity[] {
  ensureActivitiesRegistered();
  return listActivities();
}

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
            className="flex w-full items-center gap-3 rounded-[10px] px-2.5 py-[9px] text-left transition-colors hover:bg-accent/40"
          >
            <span className="flex h-[30px] w-[30px] shrink-0 items-center justify-center rounded-lg border border-border text-secondary-foreground">
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
