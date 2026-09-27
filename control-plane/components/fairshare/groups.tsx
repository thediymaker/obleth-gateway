"use client";

import Link from "next/link";
import { useMemo, useState, useTransition } from "react";
import { useRouter } from "next/navigation";
import { useQueryClient } from "@tanstack/react-query";
import { ChevronDown, ChevronRight } from "lucide-react";
import { setGroupWeightAction } from "@/app/actions";
import type { FairshareGroup, Tenant } from "@/lib/obleth";
import { GROUP_TONES, OTHER_TONE, groupShares } from "@/lib/fairshare-model";
import { Meter, Panel, Pill } from "@/components/overview/ui";
import { cn } from "@/lib/utils";
import { useFairshareLive } from "./hooks";

const pct = (n: number) => `${Math.round(n * 100)}%`;

/**
 * Group weights, with what an edit would do before it is saved: each group's
 * share of a full pool against the groups that have work, and what each is
 * using now. Edits collect in one save bar.
 */
export function FairshareGroups({ groups, tenants }: { groups: FairshareGroup[]; tenants: Tenant[] }) {
  const queryClient = useQueryClient();
  const router = useRouter();
  const live = useFairshareLive();
  const [drafts, setDrafts] = useState<Record<string, string>>({});
  const [open, setOpen] = useState<string | null>(null);
  const [errors, setErrors] = useState<string[]>([]);
  const [saved, setSaved] = useState<string | null>(null);
  const [pending, start] = useTransition();

  const liveGroups = useMemo(() => new Map((live.data?.groups ?? []).map((g) => [g.name, g])), [live.data]);
  const liveTenants = useMemo(() => new Map((live.data?.tenants ?? []).map((t) => [t.tenant_id, t])), [live.data]);
  const names = useMemo(() => [...new Set([...groups.map((g) => g.name), ...liveGroups.keys(), ...tenants.map((t) => t.fairshare_group).filter(Boolean)])], [groups, liveGroups, tenants]);
  const weightOf = (name: string) => groups.find((g) => g.name === name)?.weight ?? liveGroups.get(name)?.weight ?? 100;
  const active = (name: string) => { const g = liveGroups.get(name); return !!g && (g.in_flight > 0 || g.queued > 0); };
  const draftOf = (name: string) => {
    const raw = drafts[name];
    if (raw === undefined) return weightOf(name);
    const n = Number(raw);
    return Number.isInteger(n) && n >= 1 ? n : weightOf(name);
  };

  const before = groupShares(names.map((n) => ({ name: n, weight: weightOf(n), active: active(n) })));
  const after = groupShares(names.map((n) => ({ name: n, weight: draftOf(n), active: active(n) })));
  const changed = names.filter((n) => draftOf(n) !== weightOf(n));
  const invalid = Object.entries(drafts).filter(([, v]) => !(Number.isInteger(Number(v)) && Number(v) >= 1)).map(([n]) => n);
  const running = [...liveGroups.values()].reduce((n, g) => n + g.in_flight, 0);
  const ordered = [...names].sort((a, b) => (liveGroups.get(b)?.in_flight ?? 0) - (liveGroups.get(a)?.in_flight ?? 0) || a.localeCompare(b));
  const shareOf = (rows: typeof before, n: string) => rows.find((r) => r.name === n)?.share ?? 0;

  const save = () => start(async () => {
    setErrors([]);
    const failed: string[] = [];
    for (const name of changed) {
      const result = await setGroupWeightAction(name, draftOf(name));
      if (!result.ok) failed.push(`${name}: ${result.error}`);
    }
    setErrors(failed);
    if (!failed.length) {
      setSaved(`Saved ${changed.length} weight${changed.length === 1 ? "" : "s"}.`);
      setDrafts({});
    }
    router.refresh();
    await queryClient.invalidateQueries({ queryKey: ["fairshare-live"] });
  });

  const summary = changed.length
    ? `${changed.map((n) => `${n} ${weightOf(n)} → ${draftOf(n)}`).join(", ")}. In a full pool with these groups busy, ${names.filter((n) => shareOf(before, n) !== shareOf(after, n)).map((n) => `${n} goes from ${pct(shareOf(before, n))} to ${pct(shareOf(after, n))}`).join(", ")}. Running requests are not affected.`
    : "";

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      <div className="flex flex-col gap-1.5">
        <div className="text-[12.5px] text-muted-foreground"><Link href="/fairshare" className="hover:text-foreground">Fairshare</Link> / groups</div>
        <h1 className="text-[26px] font-semibold tracking-tight">Groups &amp; weights</h1>
        <p className="max-w-3xl text-[13.5px] text-muted-foreground">When a pool is full, its slots split between the groups active in it by group weight, then between each group&apos;s tenants by tenant weight. Groups that are not busy leave their slots for others to borrow.</p>
      </div>

      <Panel label="Groups">
        <div className="overflow-x-auto">
          <div className="min-w-[56rem]">
            <div className="grid grid-cols-[minmax(0,1fr)_9rem_10rem_minmax(0,1.3fr)_7rem_2.25rem] gap-[18px] px-[18px] pb-2 pt-4 text-[11.5px] text-muted-foreground"><span>Group</span><span>Weight</span><span>Share when all are busy</span><span>Using now, across every pool</span><span>Tenants</span><span /></div>
            {ordered.map((name, i) => {
              const g = liveGroups.get(name);
              const using = running ? (g?.in_flight ?? 0) / running : 0;
              const from = shareOf(before, name);
              const to = shareOf(after, name);
              const members = tenants.filter((t) => (t.fairshare_group || "default") === name);
              const activeMembers = members.filter((t) => { const l = liveTenants.get(t.id); return !!l && (l.in_flight > 0 || l.queued > 0); });
              const edited = draftOf(name) !== weightOf(name);
              const expanded = open === name;
              const note = !g || (!g.in_flight && !g.queued) ? "Idle" : using - to > 0.05 ? "Borrowing slots others are not using" : to - using > 0.05 ? (g.queued ? "Under its share and waiting" : "Room to grow when its tenants get busy") : "About its share";
              return (
                <div key={name} className={cn("border-t border-border", (edited || expanded) && "bg-accent/30")}>
                  <div className="grid grid-cols-[minmax(0,1fr)_9rem_10rem_minmax(0,1.3fr)_7rem_2.25rem] items-center gap-[18px] px-[18px] py-3.5">
                    <span className="flex min-w-0 items-center gap-2.5"><span className="h-2.5 w-2.5 shrink-0 rounded-sm" style={{ background: i < GROUP_TONES.length ? GROUP_TONES[i] : OTHER_TONE }} /><span className="truncate text-sm font-medium">{name}</span>{edited && <Pill inverted>Edited</Pill>}</span>
                    <span className="flex items-center gap-1.5">
                      <input aria-label={`${name} weight`} inputMode="numeric" value={drafts[name] ?? String(weightOf(name))} onChange={(e) => { setSaved(null); setDrafts((d) => ({ ...d, [name]: e.target.value.replace(/[^0-9]/g, "") })); }}
                        className="h-8 w-16 rounded-lg border border-border bg-background text-center font-mono text-[13px] outline-none focus:ring-1 focus:ring-ring" />
                      {edited && <span className="text-xs text-muted-foreground">was {weightOf(name)}</span>}
                    </span>
                    <span className="font-mono text-[13px]">{from !== to && <span className="mr-1.5 text-muted-foreground line-through">{pct(from)}</span>}{pct(to)}</span>
                    <span className="flex flex-col gap-1.5">
                      <span className="relative block"><Meter value={using} max={1} className="h-2.5" /><span className="absolute -top-1 h-[18px] w-0.5 bg-foreground" style={{ left: `${Math.min(100, to * 100)}%` }} aria-hidden="true" /></span>
                      <span className="text-xs text-muted-foreground">{pct(using)} · {note}</span>
                    </span>
                    <span className="text-[13px] text-secondary-foreground">{activeMembers.length} active <span className="text-muted-foreground">of {members.length}</span></span>
                    <button type="button" aria-label={`${expanded ? "Hide" : "Show"} ${name} tenants`} aria-expanded={expanded} onClick={() => setOpen(expanded ? null : name)} className="flex h-8 w-8 items-center justify-center rounded-lg text-muted-foreground hover:bg-accent hover:text-foreground">
                      {expanded ? <ChevronDown className="h-3.5 w-3.5" /> : <ChevronRight className="h-3.5 w-3.5" />}
                    </button>
                  </div>
                  {expanded && (
                    <div className="px-[18px] pb-3.5 pl-[46px]">
                      <div className="grid grid-cols-[minmax(0,1fr)_7rem_7rem_7rem] gap-3.5 py-1.5 text-[11.5px] text-muted-foreground"><span>Tenant</span><span>Weight</span><span>Running</span><span>Waiting</span></div>
                      {members.length === 0 && <p className="border-t border-border py-2 text-[13px] text-muted-foreground">No tenants in this group.</p>}
                      {members.sort((a, b) => (liveTenants.get(b.id)?.in_flight ?? 0) - (liveTenants.get(a.id)?.in_flight ?? 0)).map((t) => {
                        const l = liveTenants.get(t.id);
                        const idle = !l || (!l.in_flight && !l.queued);
                        return (
                          <Link key={t.id} href="/tenants" className={cn("grid grid-cols-[minmax(0,1fr)_7rem_7rem_7rem] gap-3.5 border-t border-border py-2 text-[13px] hover:bg-accent/40", idle && "text-muted-foreground")}>
                            <span className="truncate">{t.name}</span><span className="font-mono text-xs">{t.weight}</span><span className="font-mono text-xs">{l?.in_flight ?? 0}</span><span className="font-mono text-xs">{l?.queued ?? 0}</span>
                          </Link>
                        );
                      })}
                    </div>
                  )}
                </div>
              );
            })}
            {ordered.length === 0 && <p className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">No groups yet.</p>}
          </div>
        </div>
      </Panel>

      {(changed.length > 0 || invalid.length > 0 || errors.length > 0 || saved) && (
        <div className="sticky bottom-4 flex flex-wrap items-center gap-3.5 rounded-xl border border-muted-foreground/60 bg-card px-[18px] py-3.5 shadow-2xl">
          {changed.length > 0 && <Pill inverted>{changed.length} change{changed.length === 1 ? "" : "s"}</Pill>}
          <span role={errors.length ? "alert" : "status"} className="min-w-0 flex-1 text-[13px] text-secondary-foreground">
            {errors.length ? `Could not save: ${errors.join("; ")}` : invalid.length ? `Weights must be whole numbers of at least 1 (${invalid.join(", ")}).` : saved && !changed.length ? saved : summary}
          </span>
          {changed.length > 0 && (
            <>
              <button type="button" onClick={() => { setDrafts({}); setErrors([]); }} className="inline-flex h-9 items-center rounded-lg border border-border px-3.5 text-[13px] hover:bg-accent">Discard</button>
              <button type="button" disabled={pending || invalid.length > 0} onClick={save} className="inline-flex h-9 items-center rounded-lg border border-foreground bg-foreground px-3.5 text-[13px] font-medium text-background hover:bg-foreground/90 disabled:opacity-40">{pending ? "Saving…" : "Save weights"}</button>
            </>
          )}
        </div>
      )}
      <p className="text-xs text-muted-foreground">A tenant belongs to one group, set on the tenant. Shares only count the groups that have work in a pool, so a pool used by one group gives it all of it.</p>
    </div>
  );
}
