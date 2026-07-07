"use client";

import { useState } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";
import { TraceCard } from "@/components/charo/trace-card";
import type { CapabilityResult, TestOutcome, TestStatus } from "@/lib/charo/capabilities/types";

const PILL: Record<TestStatus, string> = {
  pass: "bg-emerald-500/15 text-emerald-600 dark:text-emerald-400",
  warn: "bg-amber-500/15 text-amber-600 dark:text-amber-400",
  fail: "bg-destructive/15 text-destructive",
};

function Row({ t }: { t: TestOutcome }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="border-b border-border last:border-0">
      <button type="button" onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-2 py-2 text-left">
        <span className={cn("rounded-full px-2 py-0.5 text-[10px] font-bold uppercase", PILL[t.status])}>{t.status}</span>
        <span className="text-sm">{t.label}</span>
        <span className="ml-auto truncate text-xs text-muted-foreground">{t.detail}</span>
        <ChevronRight className={cn("h-4 w-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-90")} />
      </button>
      {open && (
        <div className="space-y-2 pb-3">
          {t.output && (
            <pre className="max-h-40 overflow-auto whitespace-pre-wrap rounded-md border border-border bg-muted/40 p-2 text-xs">{t.output}</pre>
          )}
          {t.trace && <TraceCard trace={t.trace} />}
        </div>
      )}
    </div>
  );
}

export function CapabilityResultCard({ data }: { data: unknown }) {
  const r = data as Partial<CapabilityResult>;
  const tests = r.tests ?? [];
  return (
    <div className="w-full rounded-lg border border-border bg-card p-3">
      <div className="mb-1 flex items-center justify-between gap-2">
        <div className="truncate text-sm font-semibold">{r.modelName ?? "capability test"}</div>
        <span className="rounded-full border border-border px-2 py-0.5 text-[10px] uppercase tracking-wide text-muted-foreground">capability test</span>
      </div>
      <div>{tests.map((t) => <Row key={t.id} t={t} />)}</div>
    </div>
  );
}
