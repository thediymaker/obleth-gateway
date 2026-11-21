/**
 * Every dashboard server action, by area. Client components import from here;
 * each area module is a "use server" module of its own.
 */

export type { ActionResult } from "@/lib/action-result";
export * from "./tenants";
export * from "./keys";
export * from "./models";
export * from "./backup";
export * from "./mcp";
export * from "./settings";
export * from "./deployments";
export * from "./advice";
