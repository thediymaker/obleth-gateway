import { obleth, type ValidateModelBody } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

export const POST = liveRoute(async (req) => obleth.validateModel(await jsonBody<ValidateModelBody>(req)));
