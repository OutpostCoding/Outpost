// ── surface: shared ──────────────────────────────────
// Memoized markdown renderer (GFM + breaks).
// Exports: Markdown · Related: timeline/*, modals/*, FilesDrawer.tsx

import React, { memo } from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";
import remarkBreaks from "remark-breaks";
import remarkMath from "remark-math";
import rehypeKatex from "rehype-katex";
import rehypeHighlight from "rehype-highlight";
import { openUrl } from "@tauri-apps/plugin-opener";
import { nodeText, isSafeExternalUrl } from "../../utils";
import { CopyButton } from "../ui/CopyButton";

import { MermaidDiagram } from "./MermaidDiagram";


export const Markdown = memo(function Markdown({ text, streaming = false }: { text: string; streaming?: boolean }) {
  return (
    <div className="markdown">
      <ReactMarkdown
        remarkPlugins={[remarkGfm, remarkBreaks, [remarkMath, { singleDollarTextMath: false }]]}
        rehypePlugins={[rehypeKatex, rehypeHighlight]}
        components={{
          code({ node, inline, className, children, ...props }: any) {
            const match = /language-(\w+)/.exec(className || "");
            if (!inline && match && match[1] === "mermaid") {
              if (streaming) {
                return (
                  <code className={className} {...props}>
                    {children}
                  </code>
                );
              }
              const cleanChart = String(children).replace(/\n$/, "").trim();
              try {
                return <MermaidDiagram chart={cleanChart} />;
              } catch (err: any) {
                return <div style={{color: '#ff8888'}}>Mermaid parse error: {err.message || err}</div>;
              }
            }
            return (
              <code className={className} {...props}>
                {children}
              </code>
            );
          },
          a: ({ href, children }) => (
            <a
              href={href}
              onClick={(e) => {
                // Open externally — never navigate the webview away from the app.
                e.preventDefault();
                if (href && isSafeExternalUrl(href)) openUrl(href).catch(() => {});
              }}
            >
              {children}
            </a>
          ),
          pre: ({ children }) => {
            // Bypass the md-codeblock shell and duplicate outer copy button for Mermaid diagrams.
            // MermaidDiagram renders its own clean card header and actions.
            const isMermaid =
              React.isValidElement(children) &&
              (children.type === MermaidDiagram ||
                (typeof (children.props as any)?.className === "string" &&
                  (children.props as any).className.includes("language-mermaid")) ||
                (children.props as any)?.chart !== undefined);

            if (isMermaid) {
              return <>{children}</>;
            }

            return (
              <div className="md-codeblock">
                <CopyButton text={nodeText(children).replace(/\n$/, "")} className="md-copy" />
                <pre>{children}</pre>
              </div>
            );
          },
          img: ({ src, alt }) => {
            const s = typeof src === "string" ? src : "";
            if (s.startsWith("data:image/") || s.startsWith("blob:")) {
              return <img src={s} alt={alt ?? ""} />;
            }
            return (
              <a
                className="md-blocked-img"
                href={s}
                title={s}
                onClick={(e) => {
                  e.preventDefault();
                  if (s && isSafeExternalUrl(s)) openUrl(s).catch(() => {});
                }}
              >
                remote image — click to open{alt ? ` (${alt})` : ""}
              </a>
            );
          },
        }}
      >
        {text}
      </ReactMarkdown>
    </div>
  );
});
