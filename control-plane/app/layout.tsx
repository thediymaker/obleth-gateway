import { Space_Grotesk } from "next/font/google";
import "./globals.css";
import type { Metadata } from "next";
import { Providers } from "@/components/providers";
import { cn } from "@/lib/utils";

const spaceGrotesk = Space_Grotesk({
  subsets: ["latin"],
  variable: "--font-space-grotesk",
});

export const metadata: Metadata = {
  title: "obleth · Control Plane",
  description: "Fairshare AI gateway administration",
  icons: {
    icon: "/obleth.png",
    shortcut: "/obleth.png",
    apple: "/obleth.png",
  },
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={cn("dark", spaceGrotesk.variable)}>
      <body className="min-h-screen font-sans antialiased">
        <Providers>{children}</Providers>
      </body>
    </html>
  );
}
