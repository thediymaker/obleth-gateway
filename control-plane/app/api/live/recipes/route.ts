import { obleth } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

type RecipeBody = { name: string; body: string; author?: string };

export const GET = liveRoute(() => obleth.listRecipes());

export const POST = liveRoute(async (req, { session }) => obleth.createRecipe(await jsonBody<RecipeBody>(req), { auditActor: session.email }));
