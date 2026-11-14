"use client";

import { useState } from "react";
import { Plus } from "lucide-react";
import { TextArea } from "@/components/models/fields";
import { networkLines } from "@/lib/key-networks";
import type { NamedNetwork } from "@/lib/obleth";

/** Named networks offered as one-click additions. */
const OFFERED = 12;

/**
 * A key's allowed networks, one per line, with the instance's named
 * networks (Settings › Networks) offered so a range can be added by name.
 */
export function AllowedNetworksField({ defaultValue = "", named = [] }: { defaultValue?: string; named?: NamedNetwork[] }) {
  const [text, setText] = useState(defaultValue);
  const lines = networkLines(text);
  const add = (network: string) => setText((t) => (t.trim() ? `${t.replace(/\s+$/, "")}\n${network}` : network));
  return (
    <>
      <TextArea name="allowed_networks" label="Allowed networks" rows={3} value={text} onChange={setText} placeholder={"203.0.113.0/24\n2001:db8::/48"} />
      {named.length > 0 && (
        <div className="flex flex-wrap items-center gap-1.5" aria-label="Add a named network">
          <span className="text-[11.5px] text-muted-foreground">Add:</span>
          {named.slice(0, OFFERED).map((n) => {
            const listed = lines.includes(n.network);
            return (
              <button
                key={n.network}
                type="button"
                disabled={listed}
                onClick={() => add(n.network)}
                title={n.network}
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
