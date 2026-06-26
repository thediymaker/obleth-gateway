import { requireUser } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";

export const dynamic = "force-dynamic";

export default async function PortalModelsPage() {
  await requireUser();
  const models = await obleth.listModels();
  const visible = models.filter((m) => m.enabled);

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-lg font-semibold">Available models</h1>
        <p className="text-sm text-muted-foreground">
          Models available to your tenant. Contact your administrator to request
          access to additional models.
        </p>
      </div>
      {visible.length === 0 ? (
        <div className="rounded-md border border-dashed border-border px-6 py-10 text-center text-sm text-muted-foreground">
          No models are currently available.
        </div>
      ) : (
        <ul className="divide-y rounded-md border">
          {visible.map((m) => (
            <li key={m.id} className="p-3">
              <div className="font-medium">{m.model_name}</div>
              {m.description && (
                <div className="text-sm text-muted-foreground">
                  {m.description}
                </div>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
