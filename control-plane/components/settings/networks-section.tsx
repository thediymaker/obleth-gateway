"use client";

import { useState } from "react";
import { Plus, X } from "lucide-react";
import { SettingsCard } from "@/components/access/ui";
import { Setting } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { NamedNetwork, NetworkSettingsView } from "@/lib/obleth";
import { cleanNamedNetworks, NETWORK_NAME_MAX, namedNetworksProblem } from "@/lib/named-networks";

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-1 text-[12.5px] sm:grid-cols-[150px_minmax(0,1fr)]">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0">{children}</span>
    </div>
  );
}

/** Names for an instance's own address ranges, and how the gateway finds a request's client. */
export function NetworksSection({ settings }: { settings: NetworkSettingsView }) {
  const [rows, setRows] = useState<NamedNetwork[]>(() => (settings.networks.length ? settings.networks : [{ network: "", name: "" }]));
  const problem = namedNetworksProblem(rows);
  const set = (i: number, patch: Partial<NamedNetwork>) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  const ca = settings.client_address;
  return (
    <SettingsCard id="networks" title="Networks" description="Names for your own address ranges, and how the gateway finds where a request came from.">
      <Setting
        id="set-named-networks"
        label="Named networks"
        hint="Reports show these names for addresses inside them, the narrowest range first: a campus, a VPN, a cluster's pods. Names only describe; they change no check."
        fields={["networks.list"]}
      >
        <input type="hidden" name="networks.list" value={JSON.stringify(cleanNamedNetworks(rows))} />
        <ul className="flex flex-col gap-2" aria-label="Named networks">
          {rows.map((r, i) => (
            <li key={i} className="flex flex-wrap items-center gap-2">
              <Input aria-label={`Range ${i + 1}`} value={r.network} onChange={(e) => set(i, { network: e.target.value })} placeholder="10.0.0.0/8" className="h-9 w-52 font-mono text-[12.5px]" />
              <Input aria-label={`Name ${i + 1}`} value={r.name} onChange={(e) => set(i, { name: e.target.value })} placeholder="Campus" maxLength={NETWORK_NAME_MAX} className="h-9 w-60 text-[13px]" />
              <button type="button" onClick={() => setRows((rs) => (rs.length === 1 ? [{ network: "", name: "" }] : rs.filter((_, j) => j !== i)))} aria-label={`Remove ${r.network || `row ${i + 1}`}`} className="flex h-9 w-9 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground">
                <X className="h-3.5 w-3.5" />
              </button>
            </li>
          ))}
        </ul>
        <div>
          <Button type="button" variant="outline" size="sm" className="h-8" onClick={() => setRows((rs) => [...rs, { network: "", name: "" }])}>
            <Plus className="h-3.5 w-3.5" />Add a network
          </Button>
        </div>
        {problem && <span className="text-xs font-medium">{problem}</span>}
      </Setting>
      <Setting
        id="set-client-address"
        label="Where a request came from"
        hint={<>Set where the gateway is deployed: <span className="font-mono">OBLETH_TRUSTED_PROXIES</span>, <span className="font-mono">OBLETH_CLIENT_ADDRESS_HEADER</span> and <span className="font-mono">OBLETH_CLIENT_COUNTRY_HEADER</span> (Helm <span className="font-mono">obleth.clientAddress</span>).</>}
      >
        <Line label="Trusted proxies">
          <span className="font-mono text-[12px]">{ca.trusted_proxies.length ? ca.trusted_proxies.join(", ") : "none"}</span>
          {ca.trusted_proxies_default && <span className="block text-muted-foreground">The default: every private and loopback range.</span>}
          {ca.invalid_trusted_proxies.length > 0 && <span className="block font-medium">Ignored, not ranges: {ca.invalid_trusted_proxies.join(", ")}</span>}
        </Line>
        <Line label="Client address">
          {ca.address_header ? (
            <>The <span className="font-mono">{ca.address_header}</span> header from a trusted proxy, else <span className="font-mono">X-Forwarded-For</span>.</>
          ) : ca.walks_past_proxies ? (
            <><span className="font-mono">X-Forwarded-For</span> from a trusted proxy, read from the right past the trusted proxies in it.</>
          ) : (
            <>The last entry of <span className="font-mono">X-Forwarded-For</span> from a trusted proxy.</>
          )}{" "}
          From anyone else, the address that connected.
        </Line>
        <Line label="Country">
          {ca.country_header ? <>The <span className="font-mono">{ca.country_header}</span> header from a trusted proxy.</> : "Not recorded."}
        </Line>
        {ca.trusted_proxies_default && (
          <p className="text-xs text-muted-foreground">
            List your proxies to trust only them. Then a client behind two proxies, such as a tunnel and a load balancer, is found past both, and a client on a private network can&apos;t claim another address.
          </p>
        )}
      </Setting>
    </SettingsCard>
  );
}
