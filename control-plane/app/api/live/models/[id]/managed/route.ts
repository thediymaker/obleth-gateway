import { obleth, type PutManagedModel } from "@/lib/obleth";
import { jsonBody, liveRoute } from "@/lib/auth/guard";

export const GET = liveRoute<{ id: string }>((_req, { params }) => obleth.getManagedModel(params.id));

export const PUT = liveRoute<{ id: string }>(async (req, { params, session }) =>
  obleth.putManagedModel(params.id, await jsonBody<PutManagedModel>(req), { auditActor: session.email }),
);

export const DELETE = liveRoute<{ id: string }>(async (_req, { params, session }) => {
  await obleth.deleteManagedModel(params.id, { auditActor: session.email });
  return { deleted: true };
});
