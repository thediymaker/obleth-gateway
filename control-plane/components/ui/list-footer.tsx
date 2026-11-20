import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

/**
 * The line under a list that holds only part of its rows: how many are shown,
 * and a way to reach the rest. Every cut-off list ends with this, so no row is
 * hidden without a way to it. `note` says why the list stops, when it stops
 * for good (a cap) rather than a page.
 */
export function ListFooter({ shown, total, noun, more, loading, onMore, action = "Load older", note, className }: {
  shown: number;
  /** All the rows, when the list knows it (a list paged in the browser). */
  total?: number;
  /** Plural, e.g. "batches". */
  noun: string;
  /** Whether more rows can be loaded. */
  more: boolean;
  loading?: boolean;
  onMore?: () => void;
  /** The button's label. */
  action?: string;
  note?: string;
  className?: string;
}) {
  if (!more && !note) return null;
  return (
    <div className={cn("flex flex-wrap items-center justify-between gap-2 border-t border-border px-[18px] py-2.5 text-[12px] text-muted-foreground", className)}>
      <span>{shown.toLocaleString()}{total != null ? ` of ${total.toLocaleString()}` : ""} {noun} shown{note ? ` · ${note}` : ""}</span>
      {more && onMore && (
        <Button type="button" size="sm" variant="outline" className="h-8" onClick={onMore} disabled={loading}>
          {loading ? "Loading…" : action}
        </Button>
      )}
    </div>
  );
}
