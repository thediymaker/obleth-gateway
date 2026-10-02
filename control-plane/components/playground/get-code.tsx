"use client";

import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { Conversation } from "@/lib/charo/conversation";
import type { PlaygroundSession } from "./playground";
import { DEFAULT_MAX_RESULTS, domainList } from "./search-workspace";
import { buildQuestions, parseState } from "./verdicts-workspace";
import { Segmented, modelLabel } from "./ui";

type Lang = "curl" | "python" | "javascript";

/** The conversations the open chat lanes hold right now, keyed by lane index. */
function liveLanes(sessionId: string): Map<number, Conversation> {
  const detail: Record<string, unknown> = {};
  window.dispatchEvent(new CustomEvent("playground-export", { detail }));
  const out = new Map<number, Conversation>();
  for (const [key, value] of Object.entries(detail)) {
    const m = key.match(new RegExp(`:${sessionId}:compare:(\\d)$`));
    if (m && value && typeof value === "object" && Array.isArray((value as Conversation).messages)) out.set(Number(m[1]), value as Conversation);
  }
  return out;
}

/** The request this session would send, as an endpoint path and a JSON body. */
export function requestFor(session: PlaygroundSession, lane: number, lanes: Map<number, Conversation>): { path: string; body: Record<string, unknown>; note?: string } {
  if (session.mode === "image") {
    return {
      path: "/v1/images/generations",
      body: {
        model: session.imageModel ?? "IMAGE_MODEL",
        prompt: session.imagePrompt?.trim() || "A watercolour of a lighthouse at dusk",
        size: session.imageSize ?? "512x512",
        n: session.imageCount ?? 1,
        ...(session.imageNegativePrompt?.trim() ? { negative_prompt: session.imageNegativePrompt.trim() } : {}),
        ...(session.imageSteps !== undefined ? { steps: session.imageSteps } : {}),
        ...(session.imageSeed !== undefined ? { seed: session.imageSeed } : {}),
      },
    };
  }
  if (session.mode === "verdicts") {
    return {
      path: "/v1/verdicts",
      body: {
        model: session.verdictModel ?? "auto",
        state: parseState(session.verdictState ?? ""),
        questions: buildQuestions(session.verdictQuestions ?? []),
      },
    };
  }
  if (session.mode === "search") {
    const domains = domainList(session.searchDomains ?? "");
    return {
      path: "/v1/search",
      body: {
        search_tool_name: session.searchTool ?? "SEARCH_TOOL",
        query: session.searchQuery?.trim() || "attention is all you need",
        max_results: session.searchMaxResults ?? DEFAULT_MAX_RESULTS,
        ...(domains.length ? { search_domain_filter: domains } : {}),
        ...(session.searchTimeRange ? { time_range: session.searchTimeRange } : {}),
      },
    };
  }
  if (session.mode === "router") {
    return {
      path: "/v1/chat/completions",
      body: {
        model: "auto",
        messages: [{ role: "user", content: session.routerPrompt?.trim() || "Hello!" }],
        ...(session.routerMaxTokens !== undefined ? { max_tokens: session.routerMaxTokens } : {}),
      },
      note: "Sent to auto, the gateway routes it exactly as the Router tab simulates.",
    };
  }
  const g = session.generation;
  const history = (lanes.get(lane)?.messages ?? [])
    .filter((m) => !m.error && !m.workflowActivityId && !m.showLauncher && m.content.trim())
    .map((m) => ({ role: m.role, content: m.content }));
  // Reproduce the request behind the latest answer: everything up to its prompt.
  if (history.at(-1)?.role === "assistant") history.pop();
  if (!history.length) history.push({ role: "user", content: "Hello!" });
  return {
    path: "/v1/chat/completions",
    body: {
      model: session.models[lane] ?? "auto",
      messages: [...(g.systemPrompt.trim() ? [{ role: "system", content: g.systemPrompt }] : []), ...history],
      ...(g.temperature !== undefined ? { temperature: g.temperature } : {}),
      ...(g.maxTokens !== undefined ? { max_tokens: g.maxTokens } : {}),
    },
    note: history.length > 1 ? "Replays this conversation up to its latest prompt. Attached images are left out." : undefined,
  };
}

/** A JSON value as a Python literal: True/False/None, not their JSON spellings. */
function pyLiteral(v: unknown, indent: string): string {
  const inner = `${indent}    `;
  if (v === null || v === undefined) return "None";
  if (v === true) return "True";
  if (v === false) return "False";
  if (typeof v === "string" || typeof v === "number") return JSON.stringify(v);
  if (Array.isArray(v)) return v.length ? `[\n${v.map((x) => inner + pyLiteral(x, inner)).join(",\n")},\n${indent}]` : "[]";
  const entries = Object.entries(v as Record<string, unknown>);
  return entries.length ? `{\n${entries.map(([k, x]) => `${inner}${JSON.stringify(k)}: ${pyLiteral(x, inner)}`).join(",\n")},\n${indent}}` : "{}";
}

export function snippet(lang: Lang, url: string, body: Record<string, unknown>): string {
  const json = JSON.stringify(body, null, 2);
  if (lang === "curl") {
    return `curl ${url} \\\n  -H "Authorization: Bearer $OBLETH_API_KEY" \\\n  -H "Content-Type: application/json" \\\n  -d '${json.replace(/'/g, "'\\''")}'`;
  }
  if (lang === "python") {
    return `import os\nimport requests\n\nresponse = requests.post(\n    "${url}",\n    headers={"Authorization": f"Bearer {os.environ['OBLETH_API_KEY']}"},\n    json=${pyLiteral(body, "    ")},\n)\nprint(response.json())`;
  }
  return `const response = await fetch("${url}", {\n  method: "POST",\n  headers: {\n    Authorization: \`Bearer \${process.env.OBLETH_API_KEY}\`,\n    "Content-Type": "application/json",\n  },\n  body: JSON.stringify(${json.replace(/\n/g, "\n  ")}),\n});\nconsole.log(await response.json());`;
}

/** "Get code": the session's current request as something to paste into a terminal or a program. */
export function GetCode({ open, onOpenChange, session, gatewayBase }: {
  open: boolean; onOpenChange: (open: boolean) => void; session: PlaygroundSession; gatewayBase: string;
}) {
  const [lang, setLang] = useState<Lang>("curl");
  const [lane, setLane] = useState(0);
  const [lanes, setLanes] = useState<Map<number, Conversation>>(new Map());
  const [copied, setCopied] = useState(false);
  const chat = session.mode === "compare" || session.mode === "chat";
  const choices = (session.recipients ?? session.models.map((_, i) => i)).filter((i) => session.models[i] && session.models[i] !== "charo");

  useEffect(() => {
    if (!open) return;
    setLanes(liveLanes(session.id));
    setLane(choices[0] ?? 0);
    setCopied(false);
    // Snapshot when the dialog opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  const assistantOnly = chat && choices.length === 0;
  const { path, body, note } = requestFor(session, lane, lanes);
  const code = snippet(lang, `${gatewayBase.replace(/\/$/, "")}${path}`, body);
  const copy = () => void navigator.clipboard.writeText(code).then(() => { setCopied(true); setTimeout(() => setCopied(false), 1500); }).catch(() => {});

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Get code</DialogTitle>
          <DialogDescription>Send this request to the gateway yourself with any API key that can use the model.</DialogDescription>
        </DialogHeader>
        {assistantOnly ? (
          <p className="text-sm text-muted-foreground">The Assistant runs inside the control plane and has no public API. Pick a model to get code for it.</p>
        ) : (
          <div className="min-w-0 space-y-3">
            <div className="flex flex-wrap items-center gap-2">
              <Segmented label="Language" value={lang} onChange={setLang} className="w-auto" options={[{ value: "curl", label: "cURL" }, { value: "python", label: "Python" }, { value: "javascript", label: "JavaScript" }]} />
              {chat && choices.length > 1 && (
                <Segmented label="Model" value={String(lane)} onChange={(v) => setLane(Number(v))} className="w-auto" options={choices.map((i) => ({ value: String(i), label: modelLabel(session.models[i]) }))} />
              )}
            </div>
            <div className="relative">
              <pre className="max-h-[50vh] overflow-auto rounded-lg border border-border bg-background p-3 pr-12 font-mono text-xs leading-relaxed">{code}</pre>
              <Button variant="ghost" size="icon" className="absolute right-1.5 top-1.5 h-8 w-8 text-muted-foreground" aria-label="Copy code" title="Copy code" onClick={copy}>
                {copied ? <Check className="h-4 w-4" /> : <Copy className="h-4 w-4" />}
              </Button>
            </div>
            {note && <p className="text-xs text-muted-foreground">{note}</p>}
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}
