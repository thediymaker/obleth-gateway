"use client";

// Recipe editor panel for creating, editing, and copying saved recipes.
//
// Validation strategy: parseRecipe lives in sbatch-recipes.ts which imports
// node:fs at the module level. Importing it into a "use client" component would
// bundle node:fs and break the Next.js build. All validation is therefore
// server-side: saveTemplateAction runs parseRecipe and returns { ok:false, error }
// for invalid recipes, which we show in the panel. The Save button is
// only disabled while a request is in-flight (not pre-emptively on the client).

import { useState, useTransition, useEffect } from "react";
import { saveTemplateAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Notice } from "@/components/ui/notice";
import { Sheet } from "@/components/ui/sheet";

// A minimal recipe in the shipped shape: the server listens on the port obleth
// picks for each replica and serves under the deployment's name.
const RECIPE_PLACEHOLDER = [
  "---",
  "name: My model",
  "engine: vllm",
  "kind: model",
  "model: org/model",
  "model_type: chat",
  "api_model_name: my-model",
  "health_path: /health",
  "target_replicas: 1",
  "max_job_failures: 2",
  "inputs:",
  "  - name: context",
  "    label: Context length",
  "    type: number",
  "    default: 32768",
  "    required: true",
  "  - name: cache",
  "    label: Weight cache",
  "    type: path",
  '    default: "{{cluster.cache}}"',
  "    required: true",
  "---",
  "#!/bin/bash -l",
  "#SBATCH --gres=gpu:1",
  "#SBATCH --cpus-per-task=8",
  "#SBATCH --mem=48G",
  "{{cluster.setup}}",
  "set -euo pipefail",
  'export HF_HOME="{{cache}}"',
  "exec vllm serve org/model \\",
  "  --served-model-name {{api_model_name}} \\",
  '  --host 0.0.0.0 --port "$OBLETH_SERVING_PORT" \\',
  "  --max-model-len {{context}}",
].join("\n");

export interface TemplateEditorProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Pre-fill with an existing template. Omit `id` for a clone (creates new row). */
  initial?: { id?: string; name: string; body: string };
  onSaved?: () => void;
}

export function TemplateEditor({
  open,
  onOpenChange,
  initial,
  onSaved,
}: TemplateEditorProps) {
  const [name, setName] = useState(initial?.name ?? "");
  const [body, setBody] = useState(initial?.body ?? "");
  const [error, setError] = useState<string | null>(null);
  const [pending, startTransition] = useTransition();

  // Sync local state whenever the panel opens (open transitions false → true).
  // This handles the common pattern where the parent sets `initial` and `open`
  // in the same render cycle, so the textarea always shows the latest values.
  useEffect(() => {
    if (open) {
      setName(initial?.name ?? "");
      setBody(initial?.body ?? "");
      setError(null);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open]);

  function handleSave() {
    if (!name.trim()) {
      setError("Name the recipe.");
      return;
    }
    setError(null);
    startTransition(async () => {
      const res = await saveTemplateAction({ id: initial?.id, name: name.trim(), body });
      if (res.ok) {
        onOpenChange(false);
        onSaved?.();
      } else {
        setError(res.error);
      }
    });
  }

  const isNew = !initial?.id;
  const title = isNew ? "New recipe" : "Edit recipe";
  const description = isNew
    ? "Write a recipe: a header that says what it serves and the job script that serves it. It is saved on this gateway, under Saved."
    : `Editing "${initial?.name ?? initial?.id}". Changes apply only to this saved recipe.`;

  return (
    <Sheet
      open={open}
      onClose={() => { if (!pending) onOpenChange(false); }}
      title={title}
      description={description}
      width="w-[min(720px,100vw)]"
      footer={
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={() => onOpenChange(false)} disabled={pending}>
            Cancel
          </Button>
          <Button type="button" onClick={handleSave} disabled={pending}>
            {pending ? "Saving…" : "Save recipe"}
          </Button>
        </div>
      }
    >
      <div className="space-y-4 px-6 pb-6">
        <div className="space-y-1.5">
          <Label htmlFor="template-name" className="text-xs font-medium">
            Name
          </Label>
          <Input
            id="template-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            placeholder="e.g. Llama 3 8B (H100)"
            disabled={pending}
            className="h-8 text-sm"
          />
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="template-body" className="text-xs font-medium">
            Recipe body
          </Label>
          <p className="text-[11px] leading-relaxed text-muted-foreground">
            YAML frontmatter between <code className="rounded bg-secondary px-1 py-0.5 font-mono">---</code> fences,
            then the <code className="rounded bg-secondary px-1 py-0.5 font-mono">sbatch</code> script.
          </p>
          <textarea
            id="template-body"
            value={body}
            onChange={(e) => setBody(e.target.value)}
            disabled={pending}
            rows={18}
            spellCheck={false}
            className="w-full resize-y rounded-md border border-input bg-background/70 px-3 py-2 font-mono text-xs leading-relaxed shadow-sm placeholder:text-muted-foreground focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring disabled:cursor-not-allowed disabled:opacity-50"
            placeholder={RECIPE_PLACEHOLDER}
          />
        </div>

        {error && <Notice strong>{error}</Notice>}
      </div>
    </Sheet>
  );
}
