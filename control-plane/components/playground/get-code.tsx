"use client";

import { useEffect, useState } from "react";
import { Check, Copy } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import type { Conversation } from "@/lib/charo/conversation";
import type { PlaygroundSession } from "./playground";
import { DEFAULT_MAX_RESULTS, domainList } from "./search-workspace";
import { buildQuestions, parseState } from "./verdicts-workspace";
import { modelLabel } from "./ui";
import { Segmented } from "@/components/ui/segmented";

type Lang = "curl" | "python" | "javascript";

const SPEECH_PATH = "/v1/audio/speech";
const MESH_PATH = "/v1/meshes";

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
  if (session.mode === "mesh") {
    return {
      path: MESH_PATH,
      body: {
        model: session.meshModel ?? "MESH_MODEL",
        resolution: session.meshResolution ?? "1024",
        faces: session.meshFaces ?? 500_000,
        texture_size: session.meshTexture ?? 2048,
        ...(session.meshSeed !== undefined ? { seed: session.meshSeed } : {}),
      },
      note: "A job: start it with the image, poll until it completes, then download the .glb.",
    };
  }
  if (session.mode === "speech") {
    return {
      path: SPEECH_PATH,
      body: {
        model: session.speechModel ?? "SPEECH_MODEL",
        input: session.speechText?.trim() || "Hello! This is how I sound.",
        ...(session.speechVoice?.trim() ? { voice: session.speechVoice.trim() } : {}),
        ...(session.speechInstructions?.trim() ? { instructions: session.speechInstructions.trim() } : {}),
        response_format: "wav",
        stream_format: "audio",
      },
      note: "Saves the audio to speech.wav, writing it as it arrives.",
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

/**
 * Speech returns audio, not JSON, so its snippets write the body to a file as
 * it streams in. Python uses the OpenAI SDK's streaming response, which
 * requires a voice: a placeholder stands in when the session leaves it to the
 * server.
 */
export function speechSnippet(lang: Lang, gatewayBase: string, body: Record<string, unknown>): string {
  const url = `${gatewayBase}${SPEECH_PATH}`;
  if (lang === "curl") return `${snippet("curl", url, body)} \\\n  --output speech.wav`;
  if (lang === "python") {
    const { model, input, voice, ...rest } = body;
    const args = [
      `    model=${pyLiteral(model, "    ")},`,
      `    input=${pyLiteral(input, "    ")},`,
      voice === undefined ? `    voice="VOICE",  # required by the SDK: a voice the model offers` : `    voice=${pyLiteral(voice, "    ")},`,
      ...Object.entries(rest).map(([k, v]) => `    ${k}=${pyLiteral(v, "    ")},`),
    ].join("\n");
    return `import os\nfrom openai import OpenAI\n\nclient = OpenAI(\n    base_url="${gatewayBase}/v1",\n    api_key=os.environ["OBLETH_API_KEY"],\n)\n\nwith client.audio.speech.with_streaming_response.create(\n${args}\n) as response:\n    response.stream_to_file("speech.wav")`;
  }
  const json = JSON.stringify(body, null, 2);
  return `import { createWriteStream } from "node:fs";\nimport { Readable } from "node:stream";\nimport { pipeline } from "node:stream/promises";\n\nconst response = await fetch("${url}", {\n  method: "POST",\n  headers: {\n    Authorization: \`Bearer \${process.env.OBLETH_API_KEY}\`,\n    "Content-Type": "application/json",\n  },\n  body: JSON.stringify(${json.replace(/\n/g, "\n  ")}),\n});\nif (!response.ok) throw new Error(await response.text());\nawait pipeline(Readable.fromWeb(response.body), createWriteStream("speech.wav"));`;
}

/**
 * A mesh is a job with an image upload, so its snippets send a multipart form,
 * poll the job, and save the binary glTF once it completes.
 */
export function meshSnippet(lang: Lang, gatewayBase: string, body: Record<string, unknown>): string {
  const url = `${gatewayBase}${MESH_PATH}`;
  const fields = Object.entries(body);
  if (lang === "curl") {
    const form = fields.map(([k, v]) => `  -F ${k}=${String(v)} \\`).join("\n");
    return `# 1. Start the job; the answer has its id\ncurl ${url} \\\n  -H "Authorization: Bearer $OBLETH_API_KEY" \\\n${form}\n  -F image=@object.png\n\n# 2. Poll until "status" is "completed"\ncurl ${url}/$JOB_ID -H "Authorization: Bearer $OBLETH_API_KEY"\n\n# 3. Download the model\ncurl ${url}/$JOB_ID/content -H "Authorization: Bearer $OBLETH_API_KEY" -o model.glb`;
  }
  if (lang === "python") {
    const data = `{\n${fields.map(([k, v]) => `        ${JSON.stringify(k)}: ${JSON.stringify(String(v))},`).join("\n")}\n    }`;
    return `import os\nimport time\nimport requests\n\nurl = "${url}"\nheaders = {"Authorization": f"Bearer {os.environ['OBLETH_API_KEY']}"}\n\nwith open("object.png", "rb") as image:\n    job = requests.post(url, headers=headers, files={"image": image}, data=${data}).json()\n\nwhile job["status"] not in ("completed", "failed"):\n    time.sleep(5)\n    job = requests.get(f"{url}/{job['id']}", headers=headers).json()\n    print(job["status"], job.get("progress"))\n\nif job["status"] == "completed":\n    model = requests.get(f"{url}/{job['id']}/content", headers=headers)\n    with open("model.glb", "wb") as f:\n        f.write(model.content)`;
  }
  const form = fields.map(([k, v]) => `form.set(${JSON.stringify(k)}, ${JSON.stringify(String(v))});`).join("\n");
  return `import { openAsBlob } from "node:fs";\nimport { writeFile } from "node:fs/promises";\n\nconst url = "${url}";\nconst headers = { Authorization: \`Bearer \${process.env.OBLETH_API_KEY}\` };\n\nconst form = new FormData();\n${form}\nform.set("image", await openAsBlob("object.png"), "object.png");\nlet job = await (await fetch(url, { method: "POST", headers, body: form })).json();\n\nwhile (job.status !== "completed" && job.status !== "failed") {\n  await new Promise((r) => setTimeout(r, 5000));\n  job = await (await fetch(\`\${url}/\${job.id}\`, { headers })).json();\n  console.log(job.status, job.progress);\n}\n\nif (job.status === "completed") {\n  const model = await fetch(\`\${url}/\${job.id}/content\`, { headers });\n  await writeFile("model.glb", Buffer.from(await model.arrayBuffer()));\n}`;
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
  const base = gatewayBase.replace(/\/$/, "");
  const code = path === SPEECH_PATH ? speechSnippet(lang, base, body) : path === MESH_PATH ? meshSnippet(lang, base, body) : snippet(lang, `${base}${path}`, body);
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
