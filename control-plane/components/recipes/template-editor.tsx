"use client";

// Template editor dialog for creating, editing, and cloning recipe templates.
//
// Validation strategy: parseRecipe lives in sbatch-recipes.ts which imports
// node:fs at the module level. Importing it into a "use client" component would
// bundle node:fs and break the Next.js build. All validation is therefore
// server-side: saveTemplateAction runs parseRecipe and returns { ok:false, error }
// for invalid recipes, which we surface inline in the dialog. The Save button is
// only disabled while a request is in-flight (not pre-emptively on the client).

import { useState, useTransition, useEffect } from "react";
import { saveTemplateAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

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

  // Sync local state whenever the dialog opens (open transitions false → true).
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

  function handleOpenChange(next: boolean) {
    onOpenChange(next);
  }

  function handleSave() {
    if (!name.trim()) {
      setError("Template name is required.");
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
  const title = isNew ? "New template" : "Edit template";
  const description = isNew
    ? "Author a new recipe template stored in the database."
    : `Editing "${initial?.name ?? initial?.id}". Changes apply only to the saved template.`;

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className="grid max-h-[85vh] w-[min(720px,calc(100vw-2rem))] max-w-none grid-rows-[auto_minmax(0,1fr)_auto] gap-0 overflow-hidden p-0">
        <DialogHeader className="border-b border-border/70 bg-background/35 px-6 py-4 pr-12">
          <p className="mb-1 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
            Recipe template
          </p>
          <DialogTitle className="text-lg">{title}</DialogTitle>
          <DialogDescription className="mt-1">{description}</DialogDescription>
        </DialogHeader>

        <div className="min-h-0 overflow-y-auto px-6 py-4 space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="template-name" className="text-xs font-medium">
              Template name
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
            <p className="text-[11px] text-muted-foreground leading-relaxed">
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
              placeholder={"---\nname: My Template\nengine: vllm\nmodel_type: chat\napi_model_name: my-model\nport: 8000\ntarget_replicas: 2\n---\n#!/bin/bash\n# sbatch script here"}
            />
          </div>

          {error && (
            <div className="rounded-md border border-destructive/35 bg-destructive/10 px-3 py-2.5 text-sm text-destructive">
              {error}
            </div>
          )}
        </div>

        <DialogFooter className="border-t border-border/70 bg-background/35 px-6 py-3">
          <Button
            type="button"
            variant="ghost"
            onClick={() => onOpenChange(false)}
            disabled={pending}
          >
            Cancel
          </Button>
          <Button type="button" onClick={handleSave} disabled={pending}>
            {pending ? "Saving..." : "Save template"}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
