"use client";

import { useState, useTransition } from "react";
import { setQuotaAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function QuotaControl({
  id,
  tokensPerMinute,
  maxInFlight,
}: {
  id: string;
  tokensPerMinute: number;
  maxInFlight: number | null;
}) {
  const [tokens, setTokens] = useState(tokensPerMinute > 0 ? String(tokensPerMinute) : "");
  const [max, setMax] = useState(maxInFlight?.toString() ?? "");
  const [pending, start] = useTransition();

  const initialTokens = tokensPerMinute > 0 ? String(tokensPerMinute) : "";
  const changed = tokens !== initialTokens || max !== (maxInFlight?.toString() ?? "");
  const tokenValue = tokens === "" ? 0 : Number(tokens);
  const canSubmit =
    (tokens === "" || (Number.isFinite(tokenValue) && tokenValue >= 0)) &&
    (max === "" || Number(max) > 0);

  return (
    <form
      action={(fd) => start(() => setQuotaAction(fd))}
      className="flex min-w-[21rem] flex-wrap items-end gap-2"
    >
      <input type="hidden" name="id" value={id} />
      <div className="space-y-1">
        <span className="block text-[10px] uppercase tracking-wider text-muted-foreground">Tokens/min</span>
        <Input
          name="tokens_per_minute"
          type="number"
          min={0}
          value={tokens}
          onChange={(e) => setTokens(e.target.value)}
          placeholder="No cap"
          aria-label="Tokens per minute limit"
          className="w-32"
        />
      </div>
      <div className="space-y-1">
        <span className="block text-[10px] uppercase tracking-wider text-muted-foreground">In-flight</span>
        <Input
          name="max_in_flight"
          type="number"
          min={1}
          value={max}
          onChange={(e) => setMax(e.target.value)}
          placeholder="No cap"
          aria-label="Max in-flight requests"
          className="w-28"
        />
      </div>
      <Button type="submit" size="sm" variant="secondary" disabled={pending || !changed || !canSubmit}>
        {pending ? "..." : "Apply"}
      </Button>
    </form>
  );
}
