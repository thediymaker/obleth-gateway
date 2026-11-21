"use client";

import type { ReactNode } from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";

/** A one-line result under the toolbar: what a bulk action or import did. */
export function Notice({ children, onDismiss, strong }: { children: ReactNode; onDismiss?: () => void; strong?: boolean }) {
  return (
    <div role="status" className={cn("flex items-start justify-between gap-3 rounded-lg border px-3.5 py-2.5 text-[13px]", strong ? "border-foreground" : "border-border")}>
      <div className="min-w-0">{children}</div>
      {onDismiss && <button type="button" onClick={onDismiss} aria-label="Dismiss" className="shrink-0 text-muted-foreground hover:text-foreground"><X className="h-4 w-4" /></button>}
    </div>
  );
}
