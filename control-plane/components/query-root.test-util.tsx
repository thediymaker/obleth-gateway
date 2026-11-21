import type { ReactNode } from "react";
import { createRoot, type Root } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

/** A React root whose renders sit inside one react-query client, for tests of
 * components that read through useQuery. Failed reads are not retried. */
export function queryRoot(host: Element): Root {
  const root = createRoot(host);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  return {
    render: (node: ReactNode) => root.render(<QueryClientProvider client={client}>{node}</QueryClientProvider>),
    unmount: () => root.unmount(),
  } as Root;
}
