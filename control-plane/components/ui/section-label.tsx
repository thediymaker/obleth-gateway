import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** The small uppercase label over a section, a tile or a field. */
export function SectionLabel({ children, className, htmlFor }: { children: ReactNode; className?: string; htmlFor?: string }) {
  const cls = cn("text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", className);
  return htmlFor ? <label htmlFor={htmlFor} className={cls}>{children}</label> : <div className={cls}>{children}</div>;
}
