import { oidcProviderLabels } from "@/lib/auth/providers";
import { LoginForm } from "./login-form";

// OIDC_PROVIDERS is read at request time, not build time. Without this, Next
// statically prerenders the page during `next build` (when OIDC_PROVIDERS is
// unset), baking in an empty provider list so the SSO buttons never render at
// runtime — even when the env var is set in the deployed container.
export const dynamic = "force-dynamic";

export default function LoginPage() {
  const providers = oidcProviderLabels();
  return <LoginForm providers={providers} />;
}
