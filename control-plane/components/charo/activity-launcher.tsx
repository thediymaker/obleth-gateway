"use client";

import { Plus } from "lucide-react";
import {
  DropdownMenu, DropdownMenuTrigger, DropdownMenuContent, DropdownMenuItem,
} from "@/components/ui/dropdown-menu";
import type { Activity } from "@/lib/charo/activities/types";
import { ensureActivitiesRegistered, listActivities } from "@/lib/charo/activities";

function useActivities(): Activity[] {
  ensureActivitiesRegistered();
  return listActivities();
}

export function ActivityCards({ onPick }: { onPick: (a: Activity) => void }) {
  const activities = useActivities();
  return (
    <div className="w-full space-y-2">
      {activities.map((a) => {
        const Icon = a.icon;
        return (
          <button
            key={a.id}
            type="button"
            onClick={() => onPick(a)}
            className="flex w-full items-center gap-3 rounded-lg border border-border bg-secondary/20 px-3 py-2.5 text-left transition-colors hover:border-violet-400/40 hover:bg-accent/40"
          >
            <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md bg-violet-500/15 text-violet-500 dark:text-violet-300">
              {Icon && <Icon className="h-4 w-4" />}
            </span>
            <span className="min-w-0">
              <span className="block text-sm font-semibold">{a.label}</span>
              <span className="block truncate text-xs text-muted-foreground">{a.blurb}</span>
            </span>
          </button>
        );
      })}
    </div>
  );
}

export function ActivityMenu({ onPick }: { onPick: (a: Activity) => void }) {
  const activities = useActivities();
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <button type="button" title="Start an activity" className="flex h-9 w-9 items-center justify-center rounded-md border border-border text-muted-foreground hover:bg-accent/40">
          <Plus className="h-4 w-4" />
        </button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" side="top" className="z-[70] w-56">
        {activities.map((a) => (
          <DropdownMenuItem key={a.id} onSelect={() => onPick(a)} className="cursor-pointer flex-col items-start gap-0.5">
            <span className="text-sm font-medium">{a.label}</span>
            <span className="text-xs text-muted-foreground">{a.blurb}</span>
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
