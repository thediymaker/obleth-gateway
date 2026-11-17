"use client";

import { useState } from "react";
import { Plus, X } from "lucide-react";
import { SettingsCard } from "@/components/access/ui";
import { Setting } from "@/components/models/fields";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import type { NamedNetwork, NetworkSettingsView } from "@/lib/obleth";
import { cleanNamedNetworks, NETWORK_NAME_MAX, namedNetworkProblems, rangeCount, rangeLines } from "@/lib/named-networks";

function Line({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-1 text-[12.5px] sm:grid-cols-[150px_minmax(0,1fr)]">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0">{children}</span>
    </div>
  );
}

/** One network as the editor holds it: ranges as typed, one per line. */
interface Draft {
  name: string;
  text: string;
  ours: boolean;
}

const blank = (): Draft => ({ name: "", text: "", ours: false });
const toGroup = (d: Draft): NamedNetwork => ({ name: d.name, ranges: rangeLines(d.text), ours: d.ours });

/** Names for an instance's own address ranges, and how the gateway finds a request's client. */
export function NetworksSection({ settings }: { settings: NetworkSettingsView }) {
  const [drafts, setDrafts] = useState<Draft[]>(() =>
    settings.networks.length ? settings.networks.map((n) => ({ name: n.name, text: n.ranges.join("\n"), ours: !!n.ours })) : [blank()],
  );
  const groups = drafts.map(toGroup);
  const problems = namedNetworkProblems(groups);
  const problem = problems.find((p) => p !== null) ?? null;
  const clean = cleanNamedNetworks(groups);
  const ranges = rangeCount(clean);
  const set = (i: number, patch: Partial<Draft>) => setDrafts((ds) => ds.map((d, j) => (j === i ? { ...d, ...patch } : d)));
  const ca = settings.client_address;
  return (
    <SettingsCard id="networks" title="Networks" description="Names for your own address ranges, and how the gateway finds where a request came from.">
      <Setting
        id="set-named-networks"
        label="Named networks"
        hint="Reports show these names for addresses inside them: a campus, a VPN, a cluster's pods. A network can hold several ranges, and an address takes the name of the smallest range that holds it. Names only describe; they change no check."
        fields={["networks.list"]}
      >
        <input type="hidden" name="networks.list" value={JSON.stringify(clean)} />
        <ul className="flex flex-col gap-2" aria-label="Named networks">
          {drafts.map((d, i) => (
            <li key={i} className="flex flex-col gap-2 rounded-lg border border-border p-3">
              <div className="flex flex-wrap items-center gap-2">
                <Input aria-label={`Name ${i + 1}`} value={d.name} onChange={(e) => set(i, { name: e.target.value })} placeholder="Campus" maxLength={NETWORK_NAME_MAX} className="h-9 w-60 text-[13px]" />
                <label className="flex items-center gap-1.5 text-[12.5px]">
                  <input type="checkbox" aria-label={`Ours ${i + 1}`} checked={d.ours} onChange={(e) => set(i, { ours: e.target.checked })} className="accent-foreground" />
                  Ours
                </label>
                <button type="button" onClick={() => setDrafts((ds) => (ds.length === 1 ? [blank()] : ds.filter((_, j) => j !== i)))} aria-label={`Remove ${d.name.trim() || `network ${i + 1}`}`} className="ml-auto flex h-9 w-9 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground">
                  <X className="h-3.5 w-3.5" />
                </button>
              </div>
              <textarea
                aria-label={`Ranges ${i + 1}`}
                rows={Math.min(Math.max(rangeLines(d.text).length, 2), 8)}
                value={d.text}
                onChange={(e) => set(i, { text: e.target.value })}
                placeholder={"203.0.113.0/24\n2001:db8::/48"}
                autoCapitalize="none"
                autoCorrect="off"
                spellCheck={false}
                className="flex w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-[12.5px] shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
              />
              {problems[i] && <span className="text-xs font-medium">{problems[i]}</span>}
            </li>
          ))}
        </ul>
        <div className="flex flex-wrap items-center gap-3">
          <Button type="button" variant="outline" size="sm" className="h-8" onClick={() => setDrafts((ds) => [...ds, blank()])}>
            <Plus className="h-3.5 w-3.5" />Add a network
          </Button>
          <span className="text-[11.5px] text-muted-foreground">
            {clean.length} {clean.length === 1 ? "network" : "networks"}, {ranges} {ranges === 1 ? "range" : "ranges"}
          </span>
        </div>
        <p className="text-[11.5px] text-muted-foreground">One address or range per line. Tick Ours for your organization&apos;s own networks. Apps can treat use from them as expected, and they&apos;re offered first when choosing a key&apos;s allowed networks.</p>
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
          {ca.country_header ? <>The <span className="font-mono">{ca.country_header}</span> header from a trusted proxy, for a client with a public address.</> : "Not recorded."}
          {ca.countries_admit === false && (
            <span className="block font-medium">
              Recorded only. A key limited to certain countries won&apos;t accept a request by its country until your proxies are listed in <span className="font-mono">OBLETH_TRUSTED_PROXIES</span>, and those proxies must remove a <span className="font-mono">{ca.country_header ?? "CF-IPCountry"}</span> header sent by the client.
            </span>
          )}
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
