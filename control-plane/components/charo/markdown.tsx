"use client";

import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import { cn } from "@/lib/utils";

/**
 * Chat-tuned markdown for assistant messages: compact type scale (13.5px
 * body), inline code as violet mono chips, fences as inset scrollable blocks.
 * Inline-code chip styles are neutralized inside <pre> via the pre override's
 * [&_code] classes, so we don't need to detect inline-vs-block in JS.
 */
export function CharoMarkdown({ text, className }: { text: string; className?: string }) {
  return (
    <div className={cn("min-w-0 break-words text-[13.5px] leading-relaxed text-foreground/90", className)}>
      <ReactMarkdown
        remarkPlugins={[remarkGfm]}
        components={{
          p: ({ children }) => <p className="my-2 first:mt-0 last:mb-0">{children}</p>,
          strong: ({ children }) => <strong className="font-semibold text-foreground">{children}</strong>,
          a: ({ href, children }) => (
            <a href={href} target="_blank" rel="noreferrer" className="text-violet-600 underline-offset-2 hover:underline dark:text-violet-300">
              {children}
            </a>
          ),
          code: ({ children }) => (
            <code className="rounded border border-violet-500/20 bg-violet-500/10 px-1 py-px font-mono text-[12px] text-violet-700 dark:text-violet-200">
              {children}
            </code>
          ),
          pre: ({ children }) => (
            <pre className="my-2 overflow-x-auto rounded-md bg-black/5 px-2.5 py-2 text-[12px] leading-normal [&_code]:border-0 [&_code]:bg-transparent [&_code]:p-0 [&_code]:text-foreground/85 dark:bg-black/40">
              {children}
            </pre>
          ),
          ul: ({ children }) => <ul className="my-1.5 list-disc space-y-0.5 pl-4">{children}</ul>,
          ol: ({ children }) => <ol className="my-1.5 list-decimal space-y-0.5 pl-4">{children}</ol>,
          li: ({ children }) => <li className="[&>p]:my-0">{children}</li>,
          h1: ({ children }) => <p className="mb-1 mt-2 text-[13.5px] font-semibold text-foreground">{children}</p>,
          h2: ({ children }) => <p className="mb-1 mt-2 text-[13.5px] font-semibold text-foreground">{children}</p>,
          h3: ({ children }) => <p className="mb-1 mt-2 text-[13px] font-semibold text-foreground">{children}</p>,
          h4: ({ children }) => <p className="mb-1 mt-2 text-[13px] font-semibold text-foreground">{children}</p>,
          table: ({ children }) => (
            <div className="my-2 overflow-x-auto">
              <table className="w-full border-collapse text-[12px]">{children}</table>
            </div>
          ),
          th: ({ children }) => (
            <th className="border-b border-border px-2 py-1 text-left font-semibold text-foreground">{children}</th>
          ),
          td: ({ children }) => <td className="border-b border-border/50 px-2 py-1 align-top">{children}</td>,
          blockquote: ({ children }) => (
            <blockquote className="my-2 border-l-2 border-border pl-3 text-muted-foreground">{children}</blockquote>
          ),
          hr: () => <hr className="my-2 border-border/60" />,
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
}
