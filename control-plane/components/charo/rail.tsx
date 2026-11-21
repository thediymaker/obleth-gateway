import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** 10px letter-spaced label: CHARO, SOURCES · 3, CAPABILITY TEST, … */
export function MicroLabel({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div className={cn("text-[10px] font-bold uppercase tracking-[0.09em] text-muted-foreground/80", className)}>
      {children}
    </div>
  );
}

/**
 * Thin left rail that the assistant's own messages hang off. Its violet is the
 * dashboard's one colour, kept to the assistant's messages; every status mark
 * inside stays monochrome (components/ui/status.tsx).
 */
export function Rail({
  children,
  tone = "violet",
  className,
}: {
  children: ReactNode;
  tone?: "violet" | "destructive";
  className?: string;
}) {
  return (
    <div
      className={cn(
        "border-l-2 pl-3",
        tone === "destructive" ? "border-destructive/50" : "border-violet-500/45",
        className,
      )}
    >
      {children}
    </div>
  );
}
