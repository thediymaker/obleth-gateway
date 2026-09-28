"use client";

import { useEffect, useRef, useState, type ReactNode } from "react";
import { X } from "lucide-react";
import { HealthGlyph, SectionLabel } from "@/components/overview/ui";
import { STATUS_LABELS, type ModelStatus } from "@/lib/models-model";
import { providerForModel } from "@/lib/model-providers";
import { cn } from "@/lib/utils";

/**
 * A model's standing in the monochrome vocabulary: a filled dot while it
 * serves, a ring before it is checked, a dashed ring in maintenance, the
 * inverted DOWN badge when checks fail, and a plain "Off" when switched off.
 */
export function StatusMark({ status, className, label = true }: { status: ModelStatus; className?: string; label?: boolean }) {
  if (status === "off") {
    return <span className={cn("inline-flex items-center gap-2 text-[12.5px] text-muted-foreground", className)}><span aria-hidden className="inline-block h-2 w-2 rounded-[2px] border-[1.5px] border-muted-foreground/70" />{label && "Off"}</span>;
  }
  const glyph = status === "serving" ? "healthy" : status === "down" ? "unhealthy" : status === "maintenance" ? "maintenance" : "unknown";
  return (
    <span className={cn("inline-flex items-center gap-2 text-[12.5px]", status === "serving" ? "text-secondary-foreground" : "text-foreground", className)}>
      <HealthGlyph state={glyph} />
      {label && glyph !== "unhealthy" && STATUS_LABELS[status]}
    </span>
  );
}

/**
 * The model family's mark, grey, so a long list scans by shape. Falls back to
 * the name's first letter when no family is known or the image fails.
 */
export function ProviderMark({ name, upstream, size = 20, className }: { name: string; upstream?: string; size?: number; className?: string }) {
  const provider = providerForModel(name, upstream);
  const [failed, setFailed] = useState(false);
  const letter = name.replace(/[^a-z0-9]/gi, "").slice(0, 1).toUpperCase() || "?";
  return (
    <span
      aria-hidden
      title={provider?.label}
      style={{ width: size, height: size }}
      className={cn("inline-flex shrink-0 items-center justify-center overflow-hidden rounded-md border border-border bg-background", className)}
    >
      {provider && !failed ? (
        // eslint-disable-next-line @next/next/no-img-element
        <img src={provider.src} alt="" className="h-[70%] w-[70%] object-contain opacity-80 grayscale" onError={() => setFailed(true)} />
      ) : (
        <span className="text-[10px] font-semibold text-muted-foreground">{letter}</span>
      )}
    </span>
  );
}

/** A number tile; a button when it filters the list. */
export function Tile({ label, value, detail, emphasis, onClick, pressed }: {
  label: string; value: ReactNode; detail?: ReactNode; emphasis?: boolean; onClick?: () => void; pressed?: boolean;
}) {
  const cls = cn(
    "flex min-w-0 flex-col gap-1 rounded-xl border bg-card px-4 py-3 text-left transition-colors",
    emphasis ? "border-muted-foreground" : "border-border",
    onClick && "hover:border-muted-foreground/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
    pressed && "border-foreground",
  );
  const body = (
    <>
      <SectionLabel className={cn(emphasis && "text-secondary-foreground")}>{label}</SectionLabel>
      <span className="text-[24px] font-semibold leading-tight tracking-tight tabular-nums">{value}</span>
      <span className={cn("min-h-4 truncate text-xs", emphasis ? "text-foreground" : "text-muted-foreground")}>{detail ?? " "}</span>
    </>
  );
  return onClick ? <button type="button" onClick={onClick} aria-pressed={pressed} className={cls}>{body}</button> : <div className={cls}>{body}</div>;
}

/** A one-line result under the toolbar: what a bulk action or import did. */
export function Notice({ children, onDismiss, strong }: { children: ReactNode; onDismiss?: () => void; strong?: boolean }) {
  return (
    <div role="status" className={cn("flex items-start justify-between gap-3 rounded-lg border px-3.5 py-2.5 text-[13px]", strong ? "border-foreground" : "border-border")}>
      <div className="min-w-0">{children}</div>
      {onDismiss && <button type="button" onClick={onDismiss} aria-label="Dismiss" className="shrink-0 text-muted-foreground hover:text-foreground"><X className="h-4 w-4" /></button>}
    </div>
  );
}

/**
 * A panel that slides over the right of the page. Escape and the backdrop
 * close it; focus moves into it on open and back where it was on close.
 */
export function Sheet({ open, onClose, title, description, children, footer, width = "w-[min(680px,100vw)]" }: {
  open: boolean; onClose: () => void; title: string; description?: ReactNode; children: ReactNode; footer?: ReactNode; width?: string;
}) {
  const panel = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") onClose(); };
    document.addEventListener("keydown", onKey);
    const frame = requestAnimationFrame(() => panel.current?.querySelector<HTMLElement>("input:not([type=hidden]), button, textarea")?.focus());
    return () => {
      document.removeEventListener("keydown", onKey);
      cancelAnimationFrame(frame);
      previous?.focus?.();
    };
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50">
      <div className="absolute inset-0 bg-black/60" onClick={onClose} aria-hidden />
      <div ref={panel} role="dialog" aria-modal="true" aria-label={title} className={cn("absolute inset-y-0 right-0 flex flex-col border-l border-border bg-card shadow-2xl", width)}>
        <div className="flex items-start justify-between gap-4 px-6 pb-4 pt-5">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold">{title}</h2>
            {description && <p className="mt-0.5 text-[12.5px] text-muted-foreground">{description}</p>}
          </div>
          <button type="button" onClick={onClose} aria-label="Close" className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-secondary hover:text-foreground"><X className="h-4 w-4" /></button>
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
        {footer && <div className="border-t border-border bg-background/40 px-6 py-3.5">{footer}</div>}
      </div>
    </div>
  );
}
