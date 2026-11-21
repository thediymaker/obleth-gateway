import type { ReactNode } from "react";

/** A portal page's title, a line on what it is for, and its main action. */
export function PortalHeader({ title, children, action }: { title: string; children: ReactNode; action?: ReactNode }) {
  return (
    <div className="flex flex-wrap items-end justify-between gap-4">
      <div className="flex min-w-0 flex-col gap-1.5">
        <h1 className="text-[26px] font-semibold tracking-tight">{title}</h1>
        <p className="text-[13px] text-secondary-foreground">{children}</p>
      </div>
      {action}
    </div>
  );
}

/** A titled card of the portal's: a heading, a line under it, then its content. */
export function PortalCard({ title, description, children, label }: { title: string; description?: ReactNode; children: ReactNode; label?: string }) {
  return (
    <section aria-label={label ?? title} className="rounded-xl border border-border bg-card">
      <header className="px-[18px] pb-3 pt-4">
        <h2 className="text-sm font-semibold">{title}</h2>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </header>
      {children}
    </section>
  );
}

/** A chat request to the gateway, as curl and as wget, with the key read from `$OBLETH_API_KEY`. */
export function requestSnippets(gatewayBase: string, modelName: string) {
  const payload = JSON.stringify({ model: modelName, messages: [{ role: "user", content: "Say hello from obleth." }] });
  return {
    curl: [
      `curl ${gatewayBase}/v1/chat/completions`,
      `  -H "Authorization: Bearer $OBLETH_API_KEY"`,
      `  -H "Content-Type: application/json"`,
      `  -d '${payload}'`,
    ].join(" \\\n"),
    wget: [
      `wget -qO- ${gatewayBase}/v1/chat/completions`,
      `  --header="Authorization: Bearer $OBLETH_API_KEY"`,
      `  --header="Content-Type: application/json"`,
      `  --post-data='${payload}'`,
    ].join(" \\\n"),
  };
}
