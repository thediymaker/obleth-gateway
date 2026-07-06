import type { CharoTool } from "./types";
import type { CharoSettingsView } from "@/lib/obleth";

const registry = new Map<string, CharoTool>();

export function registerTool(tool: CharoTool): void {
  registry.set(tool.name, tool);
}

/** Test-only: reset the registry between cases. */
export function __clearRegistry(): void {
  registry.clear();
}

export function getTool(name: string): CharoTool | undefined {
  return registry.get(name);
}

export function isToolEnabled(settings: CharoSettingsView, name: string): boolean {
  return settings.tools_enabled?.[name] ?? true; // missing → enabled
}

export function enabledTools(settings: CharoSettingsView): CharoTool[] {
  return [...registry.values()].filter((t) => isToolEnabled(settings, t.name));
}

export function toolSchemas(settings: CharoSettingsView) {
  return enabledTools(settings).map((t) => ({
    type: "function" as const,
    function: { name: t.name, description: t.description, parameters: t.parameters },
  }));
}
