import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A preset: a name and a line on what it does. */
export interface Profile {
  key: string;
  label: ReactNode;
  blurb: ReactNode;
}

const CARD = "flex flex-col gap-0.5 rounded-lg border px-3 py-2 text-left";

/**
 * A row of preset cards; picking one sets several settings at once. `custom`
 * is the last card, for values that match no preset: a button when it can be
 * picked, otherwise a plain card that lights up when the values are custom.
 */
export function ProfileCards({ profiles, value, onPick, custom, className = "grid gap-2 lg:grid-cols-3" }: {
  profiles: readonly Profile[];
  /** The picked preset's key, or "custom". */
  value: string;
  onPick: (key: string) => void;
  custom?: { blurb: ReactNode; onPick?: () => void };
  className?: string;
}) {
  const look = (on: boolean, clickable: boolean) => cn(CARD, on ? "border-foreground bg-secondary" : cn("border-border", clickable && "hover:border-muted-foreground/60"));
  const body = (label: ReactNode, blurb: ReactNode) => (
    <>
      <span className="text-[12.5px] font-medium">{label}</span>
      <span className="text-[11.5px] leading-snug text-muted-foreground">{blurb}</span>
    </>
  );
  return (
    <div className={className}>
      {profiles.map((p) => (
        <button key={p.key} type="button" aria-pressed={value === p.key} onClick={() => onPick(p.key)} className={look(value === p.key, true)}>{body(p.label, p.blurb)}</button>
      ))}
      {custom && (custom.onPick
        ? <button type="button" aria-pressed={value === "custom"} onClick={custom.onPick} className={look(value === "custom", true)}>{body("Custom", custom.blurb)}</button>
        : <div className={look(value === "custom", false)}>{body("Custom", custom.blurb)}</div>)}
    </div>
  );
}
