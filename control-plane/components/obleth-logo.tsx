import Image from "next/image";
import { cn } from "@/lib/utils";

export function OblethLogo({ size = 28, className }: { size?: number; className?: string }) {
  return (
    <Image
      src="/obleth.png"
      alt="obleth"
      width={size}
      height={size}
      //className={cn("shrink-0 dark:invert", className)}
      className={cn("shrink-0", className)}
      priority
    />
  );
}
