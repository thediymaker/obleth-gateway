"use client";

import { Children, type KeyboardEvent, type MouseEvent, type ReactNode } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { cn } from "@/lib/utils";

export interface Column {
  label: ReactNode;
  align?: "right";
  /** Width and other classes for the column's header cell. */
  className?: string;
}

/**
 * A list as a table: one header style, one row padding, and an empty row.
 * `framed` draws it as its own card; unframed it sits inside a card or panel,
 * which draws the line above it.
 */
export function DataTable({ label, columns, minWidth, empty, footer, framed = true, children }: {
  /** The table's name for screen readers: "Models". */
  label: string;
  columns: Column[];
  /** A min-width class, so a narrow screen scrolls instead of squeezing the columns. */
  minWidth?: string;
  /** Shown as the only row when there are no rows. */
  empty?: ReactNode;
  footer?: ReactNode;
  framed?: boolean;
  children: ReactNode;
}) {
  const rows = Children.toArray(children).length;
  return (
    <div className={cn(framed && "overflow-hidden rounded-xl border border-border bg-card")}>
      <div className="overflow-x-auto">
        <table aria-label={label} className={cn("w-full text-left text-[13px]", minWidth)}>
          <thead>
            <tr className="border-b border-border text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
              {columns.map((c, i) => (
                <th key={i} scope="col" className={cn(CELL, "py-2.5 font-semibold", c.align === "right" && "text-right", c.className)}>{c.label}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {rows === 0 && empty ? (
              <tr><td colSpan={columns.length} className="px-6 py-10 text-center text-[13px] text-muted-foreground">{empty}</td></tr>
            ) : children}
          </tbody>
        </table>
      </div>
      {footer && <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-2.5 text-xs text-muted-foreground">{footer}</div>}
    </div>
  );
}

const CELL = "pr-3 first:pl-[18px] last:pr-[18px]";

/**
 * A row. With `href` or `onOpen` the whole row opens it: a click anywhere
 * but on its own links, buttons and boxes, or Enter while it has focus.
 */
export function DataRow({ href, onOpen, label, selected, muted, className, children }: {
  href?: string;
  onOpen?: () => void;
  /** What opening the row does, for screen readers: "Open chat-large". */
  label?: string;
  selected?: boolean;
  muted?: boolean;
  className?: string;
  children: ReactNode;
}) {
  const router = useRouter();
  const open = href ? () => router.push(href) : onOpen;
  const own = (target: EventTarget) => target instanceof Element && !!target.closest("a, button, input, label, select, textarea");
  return (
    <tr
      tabIndex={open ? 0 : undefined}
      aria-label={open ? label : undefined}
      aria-selected={selected || undefined}
      onClick={open ? (e: MouseEvent) => { if (!own(e.target)) open(); } : undefined}
      onKeyDown={open ? (e: KeyboardEvent) => { if (e.key === "Enter" && e.target === e.currentTarget) { e.preventDefault(); open(); } } : undefined}
      className={cn(
        "border-b border-border/70 transition-colors last:border-b-0",
        open && "cursor-pointer hover:bg-muted/30 focus-visible:bg-muted/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring",
        selected && "bg-muted/40",
        muted && "text-muted-foreground",
        className,
      )}
    >
      {children}
    </tr>
  );
}

/** One cell, with the table's padding. */
export function Cell({ align, mono, className, title, children }: { align?: "right"; mono?: boolean; className?: string; title?: string; children?: ReactNode }) {
  return <td title={title} className={cn(CELL, "py-2.5", align === "right" && "text-right", mono && "font-mono text-[12px] tabular-nums", className)}>{children}</td>;
}

/** A row's name, a link when it has a page, over a muted line such as the upstream or engine. */
export function RowTitle({ href, sub, muted, children }: { href?: string; sub?: ReactNode; muted?: boolean; children: ReactNode }) {
  const name = cn("truncate font-medium", muted ? "text-secondary-foreground" : "text-foreground");
  return (
    <span className="flex min-w-0 flex-col">
      {href ? <Link href={href} className={cn(name, "hover:underline")}>{children}</Link> : <span className={name}>{children}</span>}
      {sub && <span className="truncate font-mono text-[11.5px] text-muted-foreground">{sub}</span>}
    </span>
  );
}
