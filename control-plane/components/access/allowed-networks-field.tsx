"use client";

import { useState } from "react";
import { Plus } from "lucide-react";
import { TextArea } from "@/components/models/fields";
import { networkLines } from "@/lib/key-networks";
import { oursFirst } from "@/lib/named-networks";
import type { NamedNetwork } from "@/lib/obleth";

/** Named networks offered as one-click additions. */
const OFFERED = 12;

/**
 * A key's allowed networks, one per line, with the instance's named
 * networks (Settings › Networks) offered so a network's ranges can be added
 * by its name, our own networks first.
 */
export function AllowedNetworksField({ defaultValue = "", named = [] }: { defaultValue?: string; named?: NamedNetwork[] }) {
  const [text, setText] = useState(defaultValue);
  const lines = networkLines(text);
  const add = (ranges: string[]) => setText((t) => [t.replace(/\s+$/, ""), ...ranges].filter(Boolean).join("\n"));
  const offered = oursFirst(named.filter((n) => n.ranges.length > 0)).slice(0, OFFERED);
  return (
    <>
      <TextArea name="allowed_networks" label="Allowed networks" rows={3} value={text} onChange={setText} placeholder={"203.0.113.0/24\n2001:db8::/48"} />
      {offered.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5" aria-label="Add a named network">
          <span className="text-[11.5px] text-muted-foreground">Add:</span>
          {offered.map((n) => {
            const missing = n.ranges.filter((r) => !lines.includes(r));
            const listed = missing.length === 0;
            return (
              <button
                key={n.name}
                type="button"
                disabled={listed}
                onClick={() => add(missing)}
                title={n.ranges.join(", ")}
                className="inline-flex h-6 items-center gap-1 rounded-full border border-border px-2 text-[11.5px] text-secondary-foreground hover:bg-accent hover:text-foreground disabled:opacity-50"
              >
                {!listed && <Plus className="h-3 w-3" />}
                {n.name}
              </button>
            );
          })}
        </div>
      )}
    </>
  );
}
