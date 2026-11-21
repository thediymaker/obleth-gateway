"use client";

import { useEffect, useRef, type ReactNode } from "react";
import { X } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * A panel that slides over the right of the page. Escape and the backdrop
 * close it; focus moves into it on open and back where it was on close.
 * `header` replaces the title block (and its close button) for panels with
 * their own; `modal={false}` leaves the page behind it usable, with no backdrop.
 */
export function Sheet({ open, onClose, title, description, header, children, footer, width = "w-[min(680px,100vw)]", modal = true }: {
  open: boolean;
  onClose: () => void;
  /** The heading, and the panel's name for screen readers. */
  title: string;
  description?: ReactNode;
  header?: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  width?: string;
  modal?: boolean;
}) {
  const panel = useRef<HTMLDivElement>(null);
  // The latest onClose, read at the moment Escape is pressed. Keeping it out of
  // the effect's dependencies matters: callers often pass a new function each
  // render, and re-running the effect would pull focus back to the first field
  // (and away from whatever someone is typing in) on every parent refresh.
  const close = useRef(onClose);
  useEffect(() => {
    close.current = onClose;
  });
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") close.current(); };
    document.addEventListener("keydown", onKey);
    const frame = requestAnimationFrame(() => panel.current?.querySelector<HTMLElement>("input:not([type=hidden]), button, textarea")?.focus());
    return () => {
      document.removeEventListener("keydown", onKey);
      cancelAnimationFrame(frame);
      previous?.focus?.();
    };
  }, [open]);
  if (!open) return null;
  const body = (
    <div ref={panel} role="dialog" aria-modal={modal} aria-label={title} className={cn("inset-y-0 right-0 flex flex-col border-l border-border bg-card shadow-2xl", modal ? "absolute" : "fixed z-50", width)}>
      {header ?? (
        <div className="flex items-start justify-between gap-4 px-6 pb-4 pt-5">
          <div className="min-w-0">
            <h2 className="text-lg font-semibold">{title}</h2>
            {description && <p className="mt-0.5 text-[12.5px] text-muted-foreground">{description}</p>}
          </div>
          <SheetClose onClose={onClose} />
        </div>
      )}
      <div className="min-h-0 flex-1 overflow-y-auto">{children}</div>
      {footer && <div className="border-t border-border bg-background/40 px-6 py-3.5">{footer}</div>}
    </div>
  );
  if (!modal) return body;
  return (
    <div className="fixed inset-0 z-50">
      <div className="absolute inset-0 bg-black/60" onClick={onClose} aria-hidden />
      {body}
    </div>
  );
}

/** The sheet's close button, for a custom `header`. */
export function SheetClose({ onClose }: { onClose: () => void }) {
  return <button type="button" onClick={onClose} aria-label="Close" className="inline-flex h-8 w-8 shrink-0 items-center justify-center rounded-lg text-muted-foreground hover:bg-secondary hover:text-foreground"><X className="h-4 w-4" /></button>;
}
