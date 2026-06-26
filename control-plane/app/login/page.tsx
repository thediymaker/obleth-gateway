import { oidcProviderLabels } from "@/lib/auth/providers";
import { LoginForm } from "./login-form";

export default function LoginPage() {
  const providers = oidcProviderLabels();
  return <LoginForm providers={providers} />;
}
