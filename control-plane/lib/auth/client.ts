import { createAuthClient } from "better-auth/react";

// Configured OIDC providers (genericOAuth on the server) sign in through the
// standard signIn.social; no client plugin is needed.
export const authClient = createAuthClient();
