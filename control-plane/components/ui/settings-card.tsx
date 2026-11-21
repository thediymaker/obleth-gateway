import type { ReactNode } from "react";

/** A settings card: title, a line on what it's for, an optional action, then its settings (no body when there are none). */
export function SettingsCard({ id, title, description, children, action }: { id: string; title: string; description?: ReactNode; children: ReactNode; action?: ReactNode }) {
  return (
    <section id={id} data-section={id} aria-label={title} className="scroll-mt-24 rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">{title}</h2>
          {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
        </div>
        {action}
      </header>
      <div className="border-t border-border empty:hidden">{children}</div>
    </section>
  );
}
