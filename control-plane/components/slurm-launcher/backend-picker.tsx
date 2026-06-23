"use client";
import React from "react";
import { Cpu, PackageOpen, TerminalSquare, Zap } from "lucide-react";
import { BACKENDS, type Backend } from "@/lib/model-recipes";
import { cn } from "@/lib/utils";

const BACKEND_ICONS = {
  vllm: Zap,
  ollama: PackageOpen,
  llamacpp: Cpu,
  custom: TerminalSquare,
} as const;

// Step 2 of the launcher: pick the serving backend (vLLM / Ollama / llama.cpp /
// Custom). One card per backend; selecting one advances to the template step
// (or straight to the script editor for Custom).
export function BackendPicker(props: {
  selected?: string | null;
  onPick: (backend: Backend) => void;
}): React.ReactElement {
  return (
    <div className="grid gap-3 sm:grid-cols-2">
      {BACKENDS.map((b) => {
        const active = props.selected === b.id;
        const Icon = BACKEND_ICONS[b.id];
        return (
          <button
            key={b.id}
            type="button"
            onClick={() => props.onPick(b)}
            aria-pressed={active}
            className={cn(
              "group flex min-h-32 items-start gap-3 rounded-lg border p-4 text-left transition-colors",
              active
                ? "border-primary/50 bg-primary/10 ring-1 ring-primary/25"
                : "border-border/70 bg-background/35 hover:border-primary/45 hover:bg-muted/30",
            )}
          >
            <span
              className={cn(
                "mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-md border transition-colors",
                active
                  ? "border-primary/40 bg-primary/15 text-foreground"
                  : "border-border bg-card text-muted-foreground group-hover:text-foreground",
              )}
            >
              <Icon className="h-4 w-4" />
            </span>
            <div className="min-w-0 flex-1">
              <div className="flex w-full items-center justify-between gap-2">
                <span className="text-sm font-semibold">{b.label}</span>
                {b.badge && (
                  <span className="rounded bg-background px-1.5 py-0.5 text-[10px] text-muted-foreground">
                    {b.badge}
                  </span>
                )}
              </div>
              <span className="mt-1 block text-xs leading-relaxed text-muted-foreground">{b.blurb}</span>
            </div>
          </button>
        );
      })}
    </div>
  );
}
