"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { ChevronDown } from "lucide-react";
import { applyAdviceFixAction, restoreAdviceAction, snoozeAdviceAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { adviceLogsHref, adviceSource, fixConfirm, fixLabel, judgeLine, SNOOZE_OPTIONS, snoozedLine, splitAdvice } from "@/lib/advice-model";
import type { AdviceFindingView, AdviceReportView } from "@/lib/obleth";
import type { Finding } from "@/lib/settings-model";
import { cn } from "@/lib/utils";

const ROW = "border-t border-border px-[18px] py-3";
const LINK_BUTTON = "inline-flex h-8 items-center rounded-lg border border-border px-3 text-[12.5px] hover:border-muted-foreground/60";

/** How many things the section asks the operator to look at. */
export function needsYouCount(findings: Finding[], advice: AdviceReportView | null): number {
  return findings.length + splitAdvice(advice).needs.length;
}

/**
 * Settings > Needs you: what the last day of requests says to change (with
 * its evidence and, where there is one, a one-step fix), then the settings
 * that contradict each other.
 */
export function NeedsYouSection({ findings, advice, jump }: { findings: Finding[]; advice: AdviceReportView | null; jump: (id: string) => void }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [hidden, setHidden] = useState<string[]>([]);
  const [message, setMessage] = useState<{ key: string; text: string } | null>(null);
  const { needs, notes } = splitAdvice(advice);
  const shownNeeds = needs.filter((f) => !hidden.includes(f.key));
  const shownNotes = notes.filter((f) => !hidden.includes(f.key));
  const snoozed = (advice?.snoozed ?? []).filter((s) => !hidden.includes(`restore:${s.key}`));
  const count = findings.length + shownNeeds.length;

  const run = (key: string, act: () => Promise<{ ok: true; warnings?: string[] } | { ok: false; error: string }>, hideKey = key) =>
    start(async () => {
      const r = await act();
      if (!r.ok) { setMessage({ key, text: r.error }); return; }
      setHidden((h) => [...h, hideKey]);
      setMessage(r.warnings?.length ? { key, text: `Done, except: ${r.warnings.join("; ")}` } : null);
      router.refresh();
    });

  const apply = async (f: AdviceFindingView) => {
    if (!f.fix) return;
    const c = fixConfirm(f.fix);
    if (!(await confirm({ title: c.title, description: c.description, confirmLabel: "Turn on" }))) return;
    run(f.key, () => applyAdviceFixAction(f.key));
  };

  const card = (f: AdviceFindingView) => (
    <div key={f.key} className={cn(ROW, "flex flex-col gap-2")}>
      <div className="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-4">
        <div className="min-w-0">
          <p className="text-[13.5px] font-medium">
            {f.severity === "act" && <span className="mr-2 inline-flex h-[18px] items-center rounded-full bg-foreground px-1.5 align-[1px] text-[10.5px] font-semibold text-background">Act</span>}
            {f.title}
          </p>
          <p className="mt-0.5 text-[12.5px] text-secondary-foreground">{f.detail}</p>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {f.fix && <Button type="button" size="sm" className="h-8" disabled={pending} onClick={() => void apply(f)}>{fixLabel(f.fix)}</Button>}
          {f.logs && <Link href={adviceLogsHref(f.logs)} className={LINK_BUTTON}>See requests</Link>}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="sm" className="h-8 px-2" disabled={pending} aria-label={`Set aside: ${f.title}`}><ChevronDown className="h-3.5 w-3.5" /></Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              {SNOOZE_OPTIONS.map((o) => (
                <DropdownMenuItem key={o.label} onSelect={() => run(f.key, () => snoozeAdviceAction(f.key, f.title, o.ms ? Date.now() + o.ms : undefined))}>{o.label}</DropdownMenuItem>
              ))}
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
      {f.evidence.length > 0 && (
        <dl className="flex flex-wrap gap-x-4 gap-y-1 text-[12px]">
          {f.evidence.map((e, i) => (
            <div key={`${e.label}-${i}`} className="flex min-w-0 gap-1.5">
              <dt className="text-muted-foreground">{e.label}</dt>
              <dd className="truncate font-mono text-[11.5px] text-foreground" title={e.value}>{e.value}</dd>
            </div>
          ))}
        </dl>
      )}
      {f.judgment && <p className="text-[11.5px] text-muted-foreground">{judgeLine(f.judgment)}</p>}
      {message?.key === f.key && <p role="status" className="text-[12px] text-foreground">{message.text}</p>}
    </div>
  );

  return (
    <section id="needs" aria-label="Needs you" className={cn("scroll-mt-24 rounded-xl bg-card", count ? "border-[1.5px] border-foreground" : "border border-border")}>
      {confirmElement}
      <h2 className="px-[18px] pb-2 pt-3.5 text-sm font-semibold">{count ? `Needs you · ${count}` : "Nothing needs you"}</h2>
      {count === 0 && <p className="px-[18px] pb-3.5 text-[12.5px] text-muted-foreground">The last day of requests shows nothing to fix, alerts reach someone, and every boon a model asks for is on.</p>}

      {shownNeeds.map(card)}

      {findings.map((f) => (
        <div key={f.key} className={cn(ROW, "grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4")}>
          <div className="min-w-0"><p className="text-[13.5px] font-medium">{f.title}</p><p className="mt-0.5 text-[12.5px] text-secondary-foreground">{f.detail}</p></div>
          {f.href.startsWith("#") ? (
            <a href={f.href} onClick={(e) => { e.preventDefault(); jump(f.href.slice(1)); }} className={LINK_BUTTON}>{f.action}</a>
          ) : (
            <Link href={f.href} className={LINK_BUTTON}>{f.action}</Link>
          )}
        </div>
      ))}

      {shownNotes.length > 0 && (
        <details className="group border-t border-border">
          <summary className="cursor-pointer list-none px-[18px] py-2.5 text-[12.5px] text-muted-foreground hover:text-foreground">Also noticed · {shownNotes.length}</summary>
          {shownNotes.map(card)}
        </details>
      )}

      {snoozed.length > 0 && (
        <details className="border-t border-border">
          <summary className="cursor-pointer list-none px-[18px] py-2.5 text-[12.5px] text-muted-foreground hover:text-foreground">Set aside · {snoozed.length}</summary>
          {snoozed.map((s) => (
            <div key={s.key} className={cn(ROW, "grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4")}>
              <div className="min-w-0"><p className="text-[13px]">{s.title}</p><p className="mt-0.5 text-[12px] text-muted-foreground">{snoozedLine(s)}</p></div>
              <Button type="button" variant="outline" size="sm" className="h-8" disabled={pending} onClick={() => run(s.key, () => restoreAdviceAction(s.key), `restore:${s.key}`)}>Bring back</Button>
            </div>
          ))}
        </details>
      )}

      {advice && <p className="border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground">{adviceSource(advice)}</p>}
    </section>
  );
}
