"use client";

import { useEffect, useState, type ReactNode } from "react";
import { cn } from "@/lib/utils";
import { type StatusGlyph } from "@/components/ui/status";

export const TENANT_STATUS_LABEL: Record<string, string> = { active: "Active", suspended: "Suspended", archived: "Archived" };

/** Active is on; suspended and archived are both deliberately off. */
export function tenantGlyph(status: string): StatusGlyph {
  return status === "active" ? "on" : "off";
}

/** A secret shown once: copy it, copy it as an environment line, or close. */
export function SecretBanner({ title, secret, detail, onDone }: { title: ReactNode; secret: string; detail?: ReactNode; onDone: () => void }) {
  const [copied, setCopied] = useState<"secret" | "env" | null>(null);
  useEffect(() => {
    if (!copied) return;
    const t = setTimeout(() => setCopied(null), 1800);
    return () => clearTimeout(t);
  }, [copied]);
  const copy = (text: string, which: "secret" | "env") => {
    void navigator.clipboard?.writeText(text).then(() => setCopied(which), () => undefined);
  };
  return (
    <section aria-label="New key" className="flex flex-wrap items-center justify-between gap-4 rounded-xl border border-foreground bg-card px-[18px] py-3.5">
      <div className="flex min-w-0 flex-col gap-1.5">
        <p className="text-[13.5px] font-medium">{title}</p>
        <div className="flex min-w-0 flex-wrap items-center gap-3">
          <code className="max-w-full select-all truncate rounded-md border border-border bg-background px-2 py-1 font-mono text-[12.5px]">{secret}</code>
          {detail && <span className="text-xs text-muted-foreground">{detail}</span>}
        </div>
      </div>
      <div className="flex flex-wrap gap-2">
        <button type="button" onClick={() => copy(secret, "secret")} className="inline-flex h-9 items-center rounded-lg bg-foreground px-3 text-[13px] font-medium text-background">{copied === "secret" ? "Copied" : "Copy secret"}</button>
        <button type="button" onClick={() => copy(`OPENAI_API_KEY=${secret}`, "env")} className="inline-flex h-9 items-center rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-secondary">{copied === "env" ? "Copied" : "Copy as OPENAI_API_KEY=…"}</button>
        <button type="button" onClick={onDone} className="inline-flex h-9 items-center rounded-lg border border-border px-3 text-[13px] font-medium hover:bg-secondary">Done</button>
      </div>
    </section>
  );
}

/** A thin bar for how much of a budget is used; bright once it's near. */
export function BudgetBar({ share, className }: { share: number | null; className?: string }) {
  const pct = share == null ? 0 : Math.min(100, Math.max(0, share * 100));
  return (
    <span aria-hidden="true" className={cn("block h-1.5 overflow-hidden rounded-full bg-muted", share == null && "opacity-40", className)}>
      <span className={cn("block h-full rounded-full", (share ?? 0) >= 0.8 ? "bg-foreground" : "bg-secondary-foreground")} style={{ width: `${pct}%` }} />
    </span>
  );
}
