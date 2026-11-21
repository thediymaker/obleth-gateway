"use client";

import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import Link from "next/link";
import { Search } from "lucide-react";
import { cn } from "@/lib/utils";

/** A section in the nav: an anchor on this page, or with `href` a link to another page. */
export interface NavItem {
  id: string;
  label: ReactNode;
  count?: ReactNode;
  href?: string;
}

/** A setting "Find a setting" can jump to; `hint` is shown on the right, such as its card. */
export interface FindItem {
  id: string;
  label: string;
  words: string;
  hint?: string;
}

/**
 * Scroll to a section or setting. A setting (an id starting `set-` or `boon-`)
 * is centred, lights up for a moment, and its first control takes focus.
 */
export function jumpTo(id: string) {
  const el = document.getElementById(id);
  if (!el) return;
  const setting = id.startsWith("set-") || id.startsWith("boon-");
  el.scrollIntoView({ behavior: "smooth", block: setting ? "center" : "start" });
  if (!setting) return;
  el.classList.remove("setting-flash");
  void el.offsetWidth;
  el.classList.add("setting-flash");
  setTimeout(() => el.classList.remove("setting-flash"), 1800);
  el.querySelector<HTMLElement>("input:not([type=hidden]):not([disabled]), textarea, button[role=switch], [role=combobox], button")?.focus({ preventScroll: true });
}

/** The section nearest the top of the viewport, of the ones whose ids are given. */
export function useScrollSpy(ids: string[]): string {
  const [active, setActive] = useState(ids[0] ?? "");
  const key = ids.join(" ");
  useEffect(() => {
    const els = key.split(" ").map((id) => document.getElementById(id)).filter((e): e is HTMLElement => !!e);
    const observer = new IntersectionObserver((entries) => {
      const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
      if (visible[0]) setActive(visible[0].target.id);
    }, { rootMargin: "-20% 0px -65% 0px" });
    els.forEach((e) => observer.observe(e));
    return () => observer.disconnect();
  }, [key]);
  return active;
}

function FindSetting({ items, listId }: { items: FindItem[]; listId: string }) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q ? items.filter((s) => `${s.label} ${s.words} ${s.hint ?? ""}`.toLowerCase().includes(q)).slice(0, 6) : [];
  }, [items, query]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); input.current?.focus(); input.current?.select(); }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);
  const choose = (id: string) => { setOpen(false); setQuery(""); input.current?.blur(); jumpTo(id); };
  return (
    <div className="relative">
      <label className="flex h-9 items-center gap-2 rounded-lg border border-border bg-background px-2.5 text-[12.5px] focus-within:border-muted-foreground">
        <Search className="h-3.5 w-3.5 shrink-0 text-muted-foreground" aria-hidden />
        <input
          ref={input}
          value={query}
          onChange={(e) => { setQuery(e.target.value); setIndex(0); setOpen(true); }}
          onFocus={() => setOpen(true)}
          onBlur={() => setTimeout(() => setOpen(false), 120)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") { e.preventDefault(); setIndex((i) => Math.min(i + 1, results.length - 1)); }
            if (e.key === "ArrowUp") { e.preventDefault(); setIndex((i) => Math.max(i - 1, 0)); }
            if (e.key === "Enter" && results[index]) { e.preventDefault(); choose(results[index].id); }
            if (e.key === "Escape") { setQuery(""); input.current?.blur(); }
          }}
          placeholder="Find a setting"
          aria-label="Find a setting"
          role="combobox"
          aria-expanded={open && results.length > 0}
          aria-controls={listId}
          className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
        />
        <kbd className="rounded border border-border px-1 font-mono text-[10.5px] text-muted-foreground">⌘K</kbd>
      </label>
      {open && query.trim() !== "" && (
        <div id={listId} role="listbox" aria-label="Matching settings" className="absolute left-0 top-10 z-40 w-[min(320px,calc(100vw-2rem))] rounded-[10px] border border-border bg-chart-tooltip p-1.5 shadow-2xl">
          {results.length === 0 ? <p className="px-2.5 py-2 text-xs text-muted-foreground">No setting matches “{query}”.</p> : results.map((r, i) => (
            <button
              key={r.id}
              type="button"
              role="option"
              aria-selected={i === index}
              onMouseDown={(e) => e.preventDefault()}
              onClick={() => choose(r.id)}
              onMouseEnter={() => setIndex(i)}
              className={cn("flex w-full items-center justify-between gap-3 rounded-md px-2.5 py-2 text-left text-[13px]", i === index && "bg-secondary")}
            >
              <span className="truncate">{r.label}</span>
              {r.hint && <span className="shrink-0 font-mono text-[11px] text-muted-foreground">{r.hint}</span>}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

const ROW = "flex h-8 items-center justify-between rounded-lg px-2.5 text-[13px] transition-colors";

function NavRow({ item, active, dirty }: { item: NavItem; active: boolean; dirty: boolean }) {
  const right = <span className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">{item.count}{dirty && <span aria-label="unsaved changes" className="h-1.5 w-1.5 rounded-full bg-foreground" />}</span>;
  if (item.href) return <Link href={item.href} className={cn(ROW, "text-muted-foreground hover:bg-muted/40 hover:text-foreground")}>{item.label}{right}</Link>;
  return (
    <a
      href={`#${item.id}`}
      onClick={(e) => { e.preventDefault(); jumpTo(item.id); history.replaceState(null, "", `#${item.id}`); }}
      aria-current={active ? "true" : undefined}
      className={cn(ROW, active ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-muted/40 hover:text-foreground")}
    >
      {item.label}
      {right}
    </a>
  );
}

/**
 * The left column of a long settings page: "Find a setting" (⌘K), the
 * sections with a count or state beside each and a dot on any with unsaved
 * changes, the one in view highlighted, then `after` below a rule. It opens
 * at the section the address names.
 */
export function SettingsNav({ label, items, after = [], find, findId, dirty = [], anchor }: {
  label: string;
  items: NavItem[];
  after?: NavItem[];
  find: FindItem[];
  /** The id of the find box's result list, unique on the page. */
  findId: string;
  dirty?: string[];
  /** The section or setting to open at; the address's #hash when not given. */
  anchor?: string;
}) {
  const ids = [...items, ...after].filter((i) => !i.href).map((i) => i.id);
  const active = useScrollSpy(ids);
  useEffect(() => {
    const id = anchor || window.location.hash.slice(1);
    if (id) requestAnimationFrame(() => jumpTo(id));
  }, [anchor]);
  return (
    <nav aria-label={label} className="flex flex-col gap-0.5 lg:sticky lg:top-0">
      <div className="mb-2"><FindSetting items={find} listId={findId} /></div>
      {items.map((n) => <NavRow key={n.id} item={n} active={active === n.id} dirty={dirty.includes(n.id)} />)}
      {after.length > 0 && <div className="mx-1 my-2 h-px bg-border" />}
      {after.map((n) => <NavRow key={n.id} item={n} active={active === n.id} dirty={dirty.includes(n.id)} />)}
    </nav>
  );
}
