import { KEY_PAGE_SIZE } from "@/lib/access-model";
import { obleth, type ApiKey, type BudgetUsage, type KeyListQuery, type KeyUsageSummary } from "@/lib/obleth";
import { safe } from "@/lib/safe";

/** One page of the Keys list: its keys, their use in the window and their budgets. */
export interface KeyPageData {
  keys: ApiKey[];
  usage: KeyUsageSummary[];
  budgets: BudgetUsage[];
  /** Where the next page starts; `null` on the last. */
  next: string | null;
}

/**
 * A page of the keys `query` matches, in its order, with use and budgets for
 * those keys only: the gateway filters and sorts every key, so nothing here
 * reads more than the page.
 */
export async function loadKeyPage(query: KeyListQuery, cursor?: string): Promise<KeyPageData> {
  const page = await obleth.listKeysPage(query, { limit: KEY_PAGE_SIZE, cursor });
  const ids = page.data.map((k) => k.id);
  const capped = page.data.filter((k) => k.budget_tokens != null || k.budget_cost_usd != null).map((k) => k.id);
  const [usage, budgets] = await Promise.all([
    ids.length ? safe(obleth.usageKeysSummary({ keyIds: ids, sinceMs: query.sinceMs, untilMs: query.untilMs, limit: ids.length }), []) : [],
    capped.length ? safe(obleth.keyBudgets(capped), []) : [],
  ]);
  return { keys: page.data, usage, budgets, next: page.next };
}
