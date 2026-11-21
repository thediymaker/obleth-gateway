import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A rounded chip that is on or off, for picking several of a list. */
export function ToggleChip({ on, onClick, children, title, mono }: { on: boolean; onClick: () => void; children: ReactNode; title?: string; mono?: boolean }) {
  return (
    <button
      type="button"
      aria-pressed={on}
      title={title}
      onClick={onClick}
      className={cn(
        "inline-flex h-7 items-center rounded-full border px-2.5 transition-colors",
        mono ? "font-mono text-[11.5px]" : "text-[12px]",
        on ? "border-muted-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}
