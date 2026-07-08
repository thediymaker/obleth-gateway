"use client";

import { useState } from "react";
import { ChevronRight } from "lucide-react";
import { cn } from "@/lib/utils";
import { Rail, MicroLabel } from "@/components/charo/rail";
import { TraceCard } from "@/components/charo/trace-card";
import type { CapabilityResult, TestOutcome, TestStatus } from "@/lib/charo/capabilities/types";

const DOT: Record<TestStatus, string> = {
  pass: "bg-emerald-400",
  warn: "bg-amber-400",
  fail: "bg-destructive",
};

function Output({ text }: { text: string }) {
  const [full, setFull] = useState(false);
  const long = text.split("\n").length > 3 || text.length > 280;
  return (
    <div className="rounded-lg bg-white/[0.025] px-2.5 py-2 text-[12px] leading-normal text-muted-foreground">
      <pre className={cn("whitespace-pre-wrap font-sans", !full && long && "line-clamp-3", full && "max-h-40 overflow-auto")}>
        {text}
      </pre>
      {long && (
        <button type="button" onClick={() => setFull((f) => !f)} className="mt-1 text-[11px] text-violet-600 hover:underline dark:text-violet-300">
          {full ? "collapse" : "show full output"}
        </button>
      )}
    </div>
  );
}

function Row({ t }: { t: TestOutcome }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button type="button" onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-2 py-[5px] text-left">
        <span className={cn("h-[7px] w-[7px] shrink-0 rounded-full", DOT[t.status])} aria-label={t.status} />
        <span className="text-[13px] text-foreground/90">{t.label}</span>
        <span className="ml-auto truncate text-[11.5px] text-muted-foreground">{t.detail}</span>
        <ChevronRight className={cn("h-3.5 w-3.5 shrink-0 text-muted-foreground/70 transition-transform", open && "rotate-90")} />
      </button>
      {open && (
        <div className="mb-1.5 ml-[15px] space-y-2">
          {t.output && <Output text={t.output} />}
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
    <Rail>
      <div className="mb-1 flex items-baseline gap-2">
        <span className="truncate text-[13px] font-semibold text-foreground">{r.modelName ?? "capability test"}</span>
        <MicroLabel className="ml-auto shrink-0">Capability test</MicroLabel>
      </div>
      <div>{tests.map((t) => <Row key={t.id} t={t} />)}</div>
    </Rail>
  );
}
