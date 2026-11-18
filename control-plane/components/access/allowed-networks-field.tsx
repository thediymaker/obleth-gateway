"use client";

import { useMemo, useState } from "react";
import { Check, Plus } from "lucide-react";
import { TextArea } from "@/components/models/fields";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import { networkLines } from "@/lib/key-networks";
import { networksHolding, oursFirst, rangeKey, splitAllowedNetworks } from "@/lib/named-networks";
import type { NamedNetwork } from "@/lib/obleth";
import { cn } from "@/lib/utils";

/** Named networks shown before "Show more"; a chosen one is always shown. */
const SHOWN = 24;
/** Ranges listed in a chip's tooltip. */
const TIP_RANGES = 12;

type Field = { selected: string[]; other: string };

/** A stored list read as the named networks it holds whole, and the other lines. */
function splitField(text: string, groups: NamedNetwork[]): Field {
  const { names, rest } = splitAllowedNetworks(networkLines(text), groups);
  return { selected: names, other: rest.join("\n") };
}

/** Lines of `other` that are not one of these ranges, as typed. */
function withoutRanges(other: string, ranges: string[]): string {
  const drop = new Set(ranges.map(rangeKey));
  return other
    .split("\n")
    .filter((line) => !drop.has(rangeKey(line)))
    .join("\n");
}

/** What the form submits: each chosen network's ranges, then the other lines as typed (once each). */
function joinField(field: Field, groups: NamedNetwork[]): string {
  const ranges = groups.filter((g) => field.selected.includes(g.name)).flatMap((g) => g.ranges);
  const seen = new Set(ranges.map(rangeKey));
  const other = networkLines(field.other).filter((line) => {
    const k = rangeKey(line);
    if (seen.has(k)) return false;
    seen.add(k);
    return true;
  });
  return [...ranges, ...other].join("\n");
}

/**
 * A key's allowed networks. The instance's named networks (Settings ›
 * Networks, ours first) are chips, pressed when every one of their ranges
 * is allowed; hovering or focusing one shows its ranges. The box holds only
 * what no whole named network accounts for. The form still gets the full
 * list, one range per line, as `allowed_networks`.
 */
export function AllowedNetworksField({ defaultValue = "", named = [] }: { defaultValue?: string; named?: NamedNetwork[] }) {
  const groups = useMemo(() => oursFirst(named.filter((n) => n.ranges.length > 0)), [named]);
  const [field, setField] = useState<Field>(() => splitField(defaultValue, groups));
  const [showAll, setShowAll] = useState(false);

  const typed = new Set(networkLines(field.other).map(rangeKey));
  const isOn = (g: NamedNetwork) => field.selected.includes(g.name) || g.ranges.every((r) => typed.has(rangeKey(r)));
  const toggle = (g: NamedNetwork) =>
    setField((f) => ({
      selected: isOn(g) ? f.selected.filter((n) => n !== g.name) : [...f.selected, g.name],
      other: withoutRanges(f.other, g.ranges),
    }));

  const shown = showAll ? groups : groups.filter((g, i) => i < SHOWN || isOn(g));
  const hidden = groups.length - shown.length;
  const chips = groups.length > 0;

  return (
    <>
      <input type="hidden" name="allowed_networks" value={joinField(field, groups)} />
      {chips && (
        <TooltipProvider delayDuration={300}>
          <div role="group" aria-label="Named networks" className="flex flex-wrap items-center gap-1.5">
            {shown.map((g) => {
              const on = isOn(g);
              const inside = networksHolding(g, groups);
              return (
                <Tooltip key={g.name}>
                  <TooltipTrigger asChild>
                    <button
                      type="button"
                      aria-pressed={on}
                      onClick={() => toggle(g)}
                      className={cn(
                        "inline-flex h-6 items-center gap-1 rounded-full border px-2 text-[11.5px] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
                        on
                          ? "border-foreground/25 bg-secondary text-foreground"
                          : "border-border text-secondary-foreground hover:bg-accent hover:text-foreground",
                      )}
                    >
                      {on ? <Check className="h-3 w-3" aria-hidden /> : <Plus className="h-3 w-3" aria-hidden />}
                      {g.name}
                    </button>
                  </TooltipTrigger>
                  <TooltipContent side="bottom" className="max-w-xs">
                    <p className="font-medium">
                      {g.name} · {g.ranges.length} {g.ranges.length === 1 ? "range" : "ranges"}
                    </p>
                    {inside.length > 0 && <p className="text-muted-foreground">Inside {inside.join(" and ")}</p>}
                    <ul className="mt-1 font-mono text-[11.5px]">
                      {g.ranges.slice(0, TIP_RANGES).map((r) => (
                        <li key={r}>{r}</li>
                      ))}
                      {g.ranges.length > TIP_RANGES && <li>and {g.ranges.length - TIP_RANGES} more</li>}
                    </ul>
                  </TooltipContent>
                </Tooltip>
              );
            })}
            {hidden > 0 && (
              <button type="button" onClick={() => setShowAll(true)} className="h-6 px-1 text-[11.5px] text-muted-foreground underline-offset-2 hover:text-foreground hover:underline">
                Show {hidden} more
              </button>
            )}
          </div>
        </TooltipProvider>
      )}
      {chips && <span className="text-[11.5px] text-muted-foreground">Other addresses or ranges</span>}
      <TextArea
        label={chips ? "Other addresses or ranges" : "Allowed networks"}
        rows={chips ? 2 : 3}
        value={field.other}
        onChange={(other) => setField((f) => ({ ...f, other }))}
        placeholder={"203.0.113.0/24\n2001:db8::/48"}
      />
    </>
  );
}
