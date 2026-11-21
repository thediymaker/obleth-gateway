import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A monochrome pill. `inverted` is the one attention state; `off` marks something absent. */
export function Pill({ children, inverted, off, className }: { children: ReactNode; inverted?: boolean; off?: boolean; className?: string }) {
  return (
    <span className={cn(
      "inline-flex h-[22px] shrink-0 items-center gap-1.5 whitespace-nowrap rounded-full border px-2 text-[11.5px] font-medium",
      inverted ? "border-foreground bg-foreground text-background" : "border-border text-secondary-foreground",
      off && "border-dashed text-muted-foreground",
      className,
    )}>
      {children}
    </span>
  );
}
