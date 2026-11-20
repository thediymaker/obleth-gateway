import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

type RecipeBody = { name: string; body: string; author?: string };

export const PUT = liveRoute<{ id: string }>(async (req, { params, session }) =>
  obleth.updateRecipe(params.id, await jsonBody<RecipeBody>(req), { auditActor: session.email }),
);

export const DELETE = liveRoute<{ id: string }>(async (_req, { params, session }) => {
  await obleth.deleteRecipe(params.id, { auditActor: session.email });
  return { deleted: true };
});
