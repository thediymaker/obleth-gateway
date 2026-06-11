import { ExternalLink } from "lucide-react";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import {
  CONTROL_PLANE_SHA,
  CONTROL_PLANE_VERSION,
  fetchLatestRelease,
  isNewer,
} from "@/lib/version";
import { Badge } from "@/components/ui/badge";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";

function VersionRow({
  label,
  version,
  sha,
}: {
  label: string;
  version: string;
  sha?: string | null;
}) {
  return (
    <div className="flex items-center justify-between">
      <span className="text-muted-foreground">{label}</span>
      <span className="font-mono">
        v{version}
        {sha && (
          <span className="ml-2 text-xs text-muted-foreground">
            {sha.slice(0, 7)}
          </span>
        )}
      </span>
    </div>
  );
}

export async function VersionCard() {
  const [gateway, latest] = await Promise.all([
    safe(obleth.gatewayVersion(), null),
    fetchLatestRelease(),
  ]);

  const updateAvailable =
    latest !== null && isNewer(latest.tag, CONTROL_PLANE_VERSION);
  const versionMismatch =
    gateway !== null && gateway.version !== CONTROL_PLANE_VERSION;

  return (
    <Card>
      <CardHeader>
        <CardTitle>Version</CardTitle>
        <CardDescription>
          Installed gateway and control-plane versions, checked against the
          latest GitHub release.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2 text-sm">
          {gateway ? (
            <VersionRow
              label="Gateway"
              version={gateway.version}
              sha={gateway.git_sha}
            />
          ) : (
            <div className="flex items-center justify-between">
              <span className="text-muted-foreground">Gateway</span>
              <span className="text-muted-foreground">unreachable</span>
            </div>
          )}
          <VersionRow
            label="Control plane"
            version={CONTROL_PLANE_VERSION}
            sha={CONTROL_PLANE_SHA}
          />
          <div className="flex items-center justify-between">
            <span className="text-muted-foreground">Latest release</span>
            {latest ? (
              <a
                href={latest.url}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-1 font-mono hover:underline"
              >
                {latest.tag}
                <ExternalLink className="h-3 w-3" />
              </a>
            ) : (
              <span className="text-muted-foreground">
                update check unavailable
              </span>
            )}
          </div>
        </div>
        {updateAvailable ? (
          <Badge className="border-amber-500/50 bg-amber-500/10 text-amber-600 dark:text-amber-400">
            Update available: {latest!.tag}
          </Badge>
        ) : (
          latest && <Badge>Up to date</Badge>
        )}
        {versionMismatch && (
          <p className="text-xs text-amber-600 dark:text-amber-400">
            The gateway (v{gateway!.version}) and control plane (v
            {CONTROL_PLANE_VERSION}) are running different versions. Releases
            are published in lockstep&mdash;update both images to the same tag.
          </p>
        )}
      </CardContent>
    </Card>
  );
}
