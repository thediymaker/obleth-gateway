import { cn } from "@/lib/utils";

/** A loading placeholder block. Shape it with size and radius classes; the sweep comes from `.skeleton`. */
export function Skeleton({ className }: { className?: string }) {
  return <div aria-hidden="true" className={cn("skeleton rounded-md", className)} />;
}

/** The card frame the redesigned pages use, holding placeholder content while the page loads. */
export function SkeletonPanel({ className, children }: { className?: string; children?: React.ReactNode }) {
  return <div aria-hidden="true" className={cn("flex flex-col gap-3 rounded-xl border border-border bg-card p-[18px]", className)}>{children}</div>;
}
