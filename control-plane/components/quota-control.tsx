"use client";

import { useEffect, useState, useTransition } from "react";
import { Info, RefreshCw, Save } from "lucide-react";
import { setQuotaAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";

export function QuotaControl({
  id,
  tokensPerMinute,
  maxInFlight,
  onSaved,
}: {
  id: string;
  tokensPerMinute: number;
  maxInFlight: number | null;
  onSaved?: () => void;
}) {
  const [tokens, setTokens] = useState(tokensPerMinute > 0 ? String(tokensPerMinute) : "");
  const [max, setMax] = useState(maxInFlight?.toString() ?? "");
  const [pending, start] = useTransition();

  const initialTokens = tokensPerMinute > 0 ? String(tokensPerMinute) : "";
  const initialMax = maxInFlight?.toString() ?? "";
  useEffect(() => {
    setTokens(initialTokens);
    setMax(initialMax);
  }, [initialTokens, initialMax]);

  const changed = tokens !== initialTokens || max !== initialMax;
  const tokenValue = tokens === "" ? 0 : Number(tokens);
  const canSubmit =
    (tokens === "" || (Number.isFinite(tokenValue) && tokenValue >= 0)) &&
    (max === "" || Number(max) > 0);

  return (
    <form
      action={(fd) =>
        start(async () => {
          await setQuotaAction(fd);
          onSaved?.();
        })
      }
      className="grid min-w-0 gap-2 sm:grid-cols-[minmax(7rem,1fr)_minmax(7rem,1fr)_auto] sm:items-end"
    >
      <input type="hidden" name="id" value={id} />
      <div className="space-y-1">
        <span className="flex items-center gap-1 text-[10px] uppercase tracking-wider text-muted-foreground">
          Tokens/min
          <InfoTip>
            Sustained token-rate cap for this tenant. Clear the field and apply for unlimited token rate at the tenant level.
          </InfoTip>
        </span>
        <Input
          name="tokens_per_minute"
          type="number"
          min={0}
          value={tokens}
          onChange={(e) => setTokens(e.target.value)}
          placeholder="Unlimited"
          aria-label="Tokens per minute limit"
          title="Clear and apply to make token rate unlimited"
          className="h-8 min-w-0 text-xs"
        />
      </div>
      <div className="space-y-1">
        <span className="flex items-center gap-1 text-[10px] uppercase tracking-wider text-muted-foreground">
          Concurrency cap
          <InfoTip>
            Maximum tenant requests actively running at the same time. Clear the field and apply for unlimited tenant concurrency.
          </InfoTip>
        </span>
        <Input
          name="max_in_flight"
          type="number"
          min={1}
          value={max}
          onChange={(e) => setMax(e.target.value)}
          placeholder="Unlimited"
          aria-label="Concurrency cap"
          title="Clear and apply to make tenant concurrency unlimited"
          className="h-8 min-w-0 text-xs"
        />
      </div>
      <Button type="submit" size="sm" variant="secondary" disabled={pending || !changed || !canSubmit}>
        {pending ? (
          <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
        ) : (
          <Save className="h-3.5 w-3.5" aria-hidden />
        )}
        Apply
      </Button>
    </form>
  );
}

function InfoTip({ children }: { children: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={children}
          className="inline-flex h-3.5 w-3.5 items-center justify-center rounded-sm text-muted-foreground hover:bg-muted/30 hover:text-foreground"
        >
          <Info className="h-3 w-3" />
        </button>
      </TooltipTrigger>
      <TooltipContent side="top" align="start" className="max-w-xs normal-case tracking-normal leading-relaxed">
        {children}
      </TooltipContent>
    </Tooltip>
  );
}
