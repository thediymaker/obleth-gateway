import Link from "next/link";
import { cn } from "@/lib/utils";

export type ReportsTab = "usage" | "key-access";

const TABS: { id: ReportsTab; label: string; href: string }[] = [
  { id: "usage", label: "Usage", href: "/reports" },
  { id: "key-access", label: "Key access", href: "/reports?tab=key-access" },
];

/** The Reports page's tabs: usage and cost, and where keys are used from. */
export function ReportsTabs({ active }: { active: ReportsTab }) {
  return (
    <nav aria-label="Reports" className="flex gap-1 border-b border-border">
      {TABS.map((t) => (
        <Link
          key={t.id}
          href={t.href}
          aria-current={active === t.id ? "page" : undefined}
          className={cn(
            "-mb-px border-b-2 px-3 py-2.5 text-[13.5px]",
            active === t.id ? "border-foreground text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
          )}
        >
          {t.label}
        </Link>
      ))}
    </nav>
  );
}
