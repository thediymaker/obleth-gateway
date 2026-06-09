"use client";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { useState } from "react";

export function Providers({ children }: { children: React.ReactNode }) {
  const [client] = useState(
    () =>
      new QueryClient({
        defaultOptions: {
          queries: {
            // The dashboard already polls on explicit per-query `refetchInterval`
            // tiers, so refetching every query again on every window focus just
            // produced a burst of redundant requests. Disable focus refetching
            // and dedupe rapid remounts/navigations with a small staleTime.
            refetchOnWindowFocus: false,
            staleTime: 5_000,
            // Pause interval polling while the tab is hidden (this is the
            // default, set explicitly so it isn't accidentally re-enabled).
            refetchIntervalInBackground: false,
            retry: 1,
          },
        },
      }),
  );
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}
