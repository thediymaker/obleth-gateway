import type { ReactNode } from "react";
import { lastUsed } from "@/lib/access-model";
import { cn } from "@/lib/utils";

/** The parts of a react-query result a live read's state comes from. */
export interface LiveRead {
  isError: boolean;
  isPending: boolean;
  dataUpdatedAt: number;
  data?: unknown;
}

/** Whether a read has never answered: its first read failed and there is nothing to show. */
export function readFailed(q: LiveRead): boolean {
  return q.isError && q.data === undefined;
}

/**
 * What to say about a live read that is not current: "Can't read the usage
 * right now" when it never answered, "Last updated 3 min ago" when a refresh
 * failed and the last answer is still on screen; null when it is current or
 * still loading. A failed read never reads as "no traffic".
 */
export function readProblem(q: LiveRead, what: string, now = Date.now()): string | null {
  if (!q.isError) return null;
  if (q.data === undefined) return `Can't read ${what} right now. Retrying automatically.`;
  return `Can't refresh ${what}. Last updated ${lastUsed(q.dataUpdatedAt, now)}; retrying automatically.`;
}

/** A live read's problem as one muted line, or nothing when the read is current. */
export function ReadNotice({ query, what, className }: { query: LiveRead; what: string; className?: string }) {
  const problem = readProblem(query, what);
  if (!problem) return null;
  return <p role="alert" className={cn("text-xs text-secondary-foreground", className)}>{problem}</p>;
}

/** The box a panel shows in place of its content when its read never answered. */
export function ReadFailedNote({ query, what, className }: { query: LiveRead; what: string; className?: string }) {
  return <p role="alert" className={cn("rounded-lg border border-dashed border-border p-6 text-center text-sm text-muted-foreground", className)}>{readProblem(query, what)}</p>;
}

/**
 * A panel's body for one live read: `loading` while the first read runs,
 * "can't read" when it failed with nothing to show, else `children`, with a
 * "last updated" line above them when a later refresh failed.
 */
export function ReadState({ query, what, loading, className, children }: { query: LiveRead; what: string; loading: ReactNode; className?: string; children: ReactNode }) {
  if (readFailed(query)) return <ReadFailedNote query={query} what={what} className={className} />;
  if (query.isPending) return <>{loading}</>;
  return (
    <>
      <ReadNotice query={query} what={what} className="mb-2" />
      {children}
    </>
  );
}
