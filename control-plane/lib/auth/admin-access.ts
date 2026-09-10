import {
  APIError,
  createAuthMiddleware,
  getAuthoritativeSessionFromCtx,
} from "better-auth/api";

// The admin plugin checks roles, but knows nothing about our approval status.
// Read current database state so an existing session cannot retain access after
// an administrator marks the account pending.
export const requireActiveAccountForAdminApi = createAuthMiddleware(async (ctx) => {
  if (!ctx.path.startsWith("/admin/")) return;
  const session = await getAuthoritativeSessionFromCtx(ctx);
  const user = session?.user as { status?: string } | undefined;
  if (user?.status !== "active") {
    throw new APIError("FORBIDDEN", { message: "An active account is required" });
  }
  // The plugin still enforces its own role and permission checks. In particular,
  // stopping impersonation must remain available to an active impersonated user.
});
