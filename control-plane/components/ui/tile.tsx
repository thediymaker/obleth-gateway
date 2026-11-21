import type { ReactNode } from "react";
import Link from "next/link";
import { SectionLabel } from "@/components/ui/section-label";
import { Meter } from "@/components/overview/ui";
import { cn } from "@/lib/utils";

/** A number tile, with a meter when it is a share of something; a button when it filters the list, a link when it has a page. */
export function Tile({ label, value, detail, emphasis, onClick, pressed, href, meter }: {
  label: string; value: ReactNode; detail?: ReactNode; emphasis?: boolean; onClick?: () => void; pressed?: boolean;
  /** Used of available: drawn under the number, solid once full. */
  meter?: [number, number];
  /** Where the number's requests are: the tile becomes a link. */
  href?: string;
}) {
  const cls = cn(
    "flex min-w-0 flex-col gap-1 rounded-xl border bg-card px-4 py-3 text-left transition-colors",
    emphasis ? "border-muted-foreground" : "border-border",
    (onClick || href) && "hover:border-muted-foreground/60 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
    pressed && "border-foreground",
  );
  const body = (
    <>
      <SectionLabel className={cn(emphasis && "text-secondary-foreground")}>{label}</SectionLabel>
      <span className="text-[24px] font-semibold leading-tight tracking-tight tabular-nums">{value}</span>
      {meter && <Meter value={meter[0]} max={meter[1]} strong={meter[1] > 0 && meter[0] >= meter[1]} />}
      <span className={cn("min-h-4 truncate text-xs", emphasis ? "text-foreground" : "text-muted-foreground")}>{detail ?? " "}</span>
    </>
  );
  if (href) return <Link href={href} className={cls}>{body}</Link>;
  return onClick ? <button type="button" onClick={onClick} aria-pressed={pressed} className={cls}>{body}</button> : <div className={cls}>{body}</div>;
}
