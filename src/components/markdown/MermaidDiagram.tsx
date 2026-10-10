// ── surface: shared ──────────────────────────────────
// Interactive Mermaid diagram renderer with clean card header,
// native SVG file export, and an auto-fitting pan-and-zoom Lightbox viewer.
// Exports: MermaidDiagram, getDiagramType, getSvgDimensions, downloadSvg · Related: Markdown.tsx

import React, { useState, useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import mermaid from "mermaid";
import {
  CopyIcon,
  CheckIcon,
  DownloadIcon,
  ExpandIcon,
  ZoomInIcon,
  ZoomOutIcon,
  ResetIcon,
} from "../icons";
import { useBackdropClose } from "../../hooks/useBackdropClose";

mermaid.initialize({
  startOnLoad: false,
  theme: "dark",
  securityLevel: "strict",
});

/**
 * Inspect the first meaningful syntax line of the chart to determine
 * a human-friendly diagram category and filename suffix.
 */
export function getDiagramType(chart: string): string {
  const firstLine = chart
    .split("\n")
    .map((l) => l.trim())
    .find((l) => l && !l.startsWith("%%"));
  if (!firstLine) return "Diagram";
  if (/^(flowchart|graph)\b/i.test(firstLine)) return "Flowchart";
  if (/^sequenceDiagram\b/i.test(firstLine)) return "Sequence Diagram";
  if (/^classDiagram\b/i.test(firstLine)) return "Class Diagram";
  if (/^stateDiagram/i.test(firstLine)) return "State Diagram";
  if (/^erDiagram\b/i.test(firstLine)) return "ER Diagram";
  if (/^gantt\b/i.test(firstLine)) return "Gantt Chart";
  if (/^pie\b/i.test(firstLine)) return "Pie Chart";
  if (/^gitGraph\b/i.test(firstLine)) return "Git Graph";
  if (/^mindmap\b/i.test(firstLine)) return "Mindmap";
  if (/^timeline\b/i.test(firstLine)) return "Timeline";
  if (/^journey\b/i.test(firstLine)) return "User Journey";
  if (/^c4/i.test(firstLine)) return "C4 Architecture";
  return "Mermaid Diagram";
}

/**
 * Extracts intrinsic width and height from the SVG's viewBox or width/height attributes.
 */
export function getSvgDimensions(svgCode: string): { width: number; height: number } {
  const vbMatch = svgCode.match(
    /viewBox=["']\s*([-\d.]+)\s+([-\d.]+)\s+([-\d.]+)\s+([-\d.]+)\s*["']/i
  );
  if (vbMatch) {
    const w = parseFloat(vbMatch[3]);
    const h = parseFloat(vbMatch[4]);
    if (!isNaN(w) && !isNaN(h) && w > 0 && h > 0) {
      return { width: Math.round(w), height: Math.round(h) };
    }
  }

  const wMatch = svgCode.match(/\bwidth=["']\s*([\d.]+)(?:px)?\s*["']/i);
  const hMatch = svgCode.match(/\bheight=["']\s*([\d.]+)(?:px)?\s*["']/i);
  if (wMatch && hMatch) {
    const w = parseFloat(wMatch[1]);
    const h = parseFloat(hMatch[1]);
    if (!isNaN(w) && !isNaN(h) && w > 0 && h > 0) {
      return { width: Math.round(w), height: Math.round(h) };
    }
  }

  return { width: 800, height: 600 };
}

/**
 * Triggers a native Windows file dialog or browser fallback to save the pure vector SVG.
 */
export async function downloadSvg(
  svgContent: string,
  filename = "diagram.svg"
): Promise<boolean> {
  // If running in Tauri desktop, use native file save dialog
  if (typeof window !== "undefined" && "__TAURI_INTERNALS__" in window) {
    try {
      const { save } = await import("@tauri-apps/plugin-dialog");
      const { invoke } = await import("@tauri-apps/api/core");
      const selected = await save({
        defaultPath: filename,
        filters: [{ name: "SVG Image", extensions: ["svg"] }],
      });
      if (selected) {
        await invoke("save_file", { path: selected, content: svgContent });
        return true;
      }
      return false;
    } catch (err) {
      console.warn("Native file save dialog failed:", err);
    }
  }

  // Browser fallback for tests / web environments
  try {
    const blob = new Blob([svgContent], { type: "image/svg+xml;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
    return true;
  } catch {
    return false;
  }
}

interface LightboxModalProps {
  svgCode: string;
  chart: string;
  diagramType: string;
  onClose: () => void;
}

function MermaidLightboxModal({
  svgCode,
  chart,
  diagramType,
  onClose,
}: LightboxModalProps) {
  const { width: intrinsicWidth, height: intrinsicHeight } = getSvgDimensions(svgCode);

  const [zoom, setZoom] = useState(1);
  const [fitZoom, setFitZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [isDragging, setIsDragging] = useState(false);
  const [copiedSvg, setCopiedSvg] = useState(false);
  const [copiedCode, setCopiedCode] = useState(false);
  const [downloadSaved, setDownloadSaved] = useState(false);

  const canvasRef = useRef<HTMLDivElement>(null);
  const dragStartRef = useRef({ x: 0, y: 0 });
  const panStartRef = useRef({ x: 0, y: 0 });

  const backdropHandlers = useBackdropClose(onClose);

  // Calculate optimal initial fit scale based on viewport size vs intrinsic diagram dimensions
  useEffect(() => {
    const calculateFit = () => {
      if (!canvasRef.current) return;
      const rect = canvasRef.current.getBoundingClientRect();
      const availW = Math.max(100, rect.width - 80);
      const availH = Math.max(100, rect.height - 80);

      const scaleW = availW / intrinsicWidth;
      const scaleH = availH / intrinsicHeight;
      // Fit scale fits the whole diagram comfortably; cap max initial zoom at 1.0 so small diagrams aren't blown up
      const calculatedFit = Number(Math.min(scaleW, scaleH, 1.0).toFixed(2));
      const initialScale = Math.max(0.15, calculatedFit);

      setFitZoom(initialScale);
      setZoom(initialScale);
      setPan({ x: 0, y: 0 });
    };

    calculateFit();
    window.addEventListener("resize", calculateFit);
    return () => window.removeEventListener("resize", calculateFit);
  }, [intrinsicWidth, intrinsicHeight]);

  // Close with Esc, adjust zoom with +/-/0
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        onClose();
      } else if (e.key === "+" || e.key === "=") {
        setZoom((z) => Math.min(5, Number((z * 1.25).toFixed(2))));
      } else if (e.key === "-") {
        setZoom((z) => Math.max(0.15, Number((z / 1.25).toFixed(2))));
      } else if (e.key === "0") {
        setZoom(fitZoom);
        setPan({ x: 0, y: 0 });
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [onClose, fitZoom]);

  const handleWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    e.stopPropagation();
    const factor = e.deltaY < 0 ? 1.15 : 0.87;
    setZoom((prev) => Math.min(5, Math.max(0.15, Number((prev * factor).toFixed(2)))));
  };

  const handlePointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0) return;
    setIsDragging(true);
    dragStartRef.current = { x: e.clientX, y: e.clientY };
    panStartRef.current = { ...pan };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  };

  const handlePointerMove = (e: React.PointerEvent) => {
    if (!isDragging) return;
    const dx = e.clientX - dragStartRef.current.x;
    const dy = e.clientY - dragStartRef.current.y;
    setPan({
      x: panStartRef.current.x + dx,
      y: panStartRef.current.y + dy,
    });
  };

  const handlePointerUp = (e: React.PointerEvent) => {
    if (isDragging) {
      setIsDragging(false);
      try {
        (e.currentTarget as HTMLElement).releasePointerCapture(e.pointerId);
      } catch {}
    }
  };

  const handleResetToFit = () => {
    setZoom(fitZoom);
    setPan({ x: 0, y: 0 });
  };

  const handleDownload = async () => {
    const slug = diagramType.toLowerCase().replace(/[^a-z0-9]+/g, "-");
    const saved = await downloadSvg(svgCode, `diagram-${slug}.svg`);
    if (saved) {
      setDownloadSaved(true);
      setTimeout(() => setDownloadSaved(false), 2500);
    }
  };

  const handleCopySvg = () => {
    navigator.clipboard.writeText(svgCode).then(() => {
      setCopiedSvg(true);
      setTimeout(() => setCopiedSvg(false), 2000);
    });
  };

  const handleCopyCode = () => {
    navigator.clipboard.writeText(chart).then(() => {
      setCopiedCode(true);
      setTimeout(() => setCopiedCode(false), 2000);
    });
  };

  return createPortal(
    <div className="mermaid-lightbox-overlay" {...backdropHandlers}>
      <div
        className="mermaid-lightbox-dialog"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        {/* Header toolbar */}
        <div className="mermaid-lightbox-header">
          <div className="mermaid-lightbox-title-group">
            <span className="mermaid-lightbox-badge">{diagramType}</span>
            <span className="mermaid-lightbox-zoom-label">
              {Math.round(zoom * 100)}%
            </span>
          </div>

          <div className="mermaid-lightbox-controls">
            <button
              className="mermaid-tool-btn"
              onClick={() => setZoom((z) => Math.max(0.15, Number((z / 1.25).toFixed(2))))}
              title="Zoom out (-)"
              aria-label="Zoom out"
            >
              <ZoomOutIcon />
            </button>
            <button
              className="mermaid-tool-btn"
              onClick={() => setZoom((z) => Math.min(5, Number((z * 1.25).toFixed(2))))}
              title="Zoom in (+)"
              aria-label="Zoom in"
            >
              <ZoomInIcon />
            </button>
            <button
              className="mermaid-tool-btn"
              onClick={handleResetToFit}
              title="Fit to view (0)"
              aria-label="Fit to view"
            >
              <ResetIcon />
              <span className="mermaid-btn-label">Fit</span>
            </button>
            <button
              className="mermaid-tool-btn"
              onClick={() => {
                setZoom(1);
                setPan({ x: 0, y: 0 });
              }}
              title="100% natural authoring scale"
              aria-label="100% natural scale"
            >
              <span className="mermaid-btn-label">1:1</span>
            </button>

            <div className="mermaid-tool-divider" />

            <button
              className="mermaid-tool-btn"
              onClick={handleCopyCode}
              title="Copy Mermaid source syntax"
              aria-label="Copy Mermaid syntax"
            >
              {copiedCode ? <CheckIcon /> : <CopyIcon />}
              <span className="mermaid-btn-label">{copiedCode ? "Copied" : "Copy Code"}</span>
            </button>
            <button
              className="mermaid-tool-btn"
              onClick={handleCopySvg}
              title="Copy SVG markup"
              aria-label="Copy SVG"
            >
              {copiedSvg ? <CheckIcon /> : <CopyIcon />}
              <span className="mermaid-btn-label">{copiedSvg ? "Copied" : "Copy SVG"}</span>
            </button>
            <button
              className="mermaid-tool-btn mermaid-tool-btn-primary"
              onClick={handleDownload}
              title="Save vector SVG to disk"
              aria-label="Save SVG"
            >
              {downloadSaved ? <CheckIcon /> : <DownloadIcon />}
              <span className="mermaid-btn-label">{downloadSaved ? "Saved" : "Save SVG"}</span>
            </button>

            <button
              className="mermaid-tool-btn mermaid-close-btn"
              onClick={onClose}
              title="Close (Esc)"
              aria-label="Close lightbox"
            >
              ✕
            </button>
          </div>
        </div>

        {/* Interactive canvas */}
        <div
          ref={canvasRef}
          className="mermaid-lightbox-canvas"
          onWheel={handleWheel}
          onPointerDown={handlePointerDown}
          onPointerMove={handlePointerMove}
          onPointerUp={handlePointerUp}
          onPointerCancel={handlePointerUp}
          onDoubleClick={handleResetToFit}
          style={{ cursor: isDragging ? "grabbing" : "grab" }}
        >
          <div
            className="mermaid-lightbox-stage"
            style={{
              width: `${intrinsicWidth}px`,
              height: `${intrinsicHeight}px`,
              transform: `translate3d(${pan.x}px, ${pan.y}px, 0) scale(${zoom})`,
              transformOrigin: "center center",
              transition: isDragging ? "none" : "transform 0.08s ease-out",
            }}
          >
            <div
              className="mermaid-lightbox-svg"
              dangerouslySetInnerHTML={{ __html: svgCode }}
            />
          </div>
        </div>

        {/* Footer tip */}
        <div className="mermaid-lightbox-footer">
          <span>Scroll wheel to zoom · Click & drag to pan · Double-click to reset · Esc to close</span>
        </div>
      </div>
    </div>,
    document.body
  );
}

export function MermaidDiagram({ chart }: { chart: string }) {
  const [svgCode, setSvgCode] = useState<string>("");
  const [error, setError] = useState<string | null>(null);
  const [isLightboxOpen, setIsLightboxOpen] = useState(false);
  const [copied, setCopied] = useState(false);
  const [saved, setSaved] = useState(false);

  const cleanChart = chart.trim();
  const diagramType = getDiagramType(cleanChart);

  useEffect(() => {
    let active = true;
    const id = `mermaid-${Math.random().toString(36).substring(2)}`;

    try {
      mermaid
        .render(id, cleanChart)
        .then(({ svg }) => {
          if (active) {
            setSvgCode(svg);
            setError(null);
          }
        })
        .catch((e) => {
          console.error("Mermaid error:", e);
          document.getElementById(id)?.remove();
          document.getElementById("d" + id)?.remove();
          document
            .querySelectorAll('body > svg[id^="dmermaid-"], body > [id^="mermaid-"]')
            .forEach((el) => el.remove());
          if (active) {
            setError((e.message || e).toString());
          }
        });
    } catch (e: any) {
      console.error("Mermaid sync error:", e);
      document.getElementById(id)?.remove();
      document.getElementById("d" + id)?.remove();
      document
        .querySelectorAll('body > svg[id^="dmermaid-"], body > [id^="mermaid-"]')
        .forEach((el) => el.remove());
      if (active) {
        setError((e.message || e).toString());
      }
    }

    return () => {
      active = false;
    };
  }, [cleanChart]);

  if (error) {
    return (
      <div className="mermaid-error-box">
        <strong>Mermaid Syntax Error:</strong>
        <br />
        <br />
        {error}
      </div>
    );
  }

  if (!svgCode) {
    return (
      <div className="mermaid-loading-box">
        <span>Rendering {diagramType.toLowerCase()}...</span>
      </div>
    );
  }

  const handleCopy = (e: React.MouseEvent) => {
    e.stopPropagation();
    navigator.clipboard.writeText(cleanChart).then(() => {
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    });
  };

  const handleDownload = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const slug = diagramType.toLowerCase().replace(/[^a-z0-9]+/g, "-");
    const ok = await downloadSvg(svgCode, `diagram-${slug}.svg`);
    if (ok) {
      setSaved(true);
      setTimeout(() => setSaved(false), 2500);
    }
  };

  return (
    <>
      <div className="mermaid-card">
        {/* Dedicated card header sitting ABOVE the diagram canvas (outside image) */}
        <div className="mermaid-card-header">
          <span className="mermaid-card-type-tag">{diagramType}</span>
          <div className="mermaid-card-actions">
            <button
              className="mermaid-card-btn"
              onClick={handleCopy}
              title="Copy diagram source"
              aria-label="Copy diagram source"
            >
              {copied ? <CheckIcon /> : <CopyIcon />}
              <span className="mermaid-card-btn-text">{copied ? "Copied" : "Copy"}</span>
            </button>
            <button
              className="mermaid-card-btn"
              onClick={handleDownload}
              title="Save SVG to disk"
              aria-label="Save SVG"
            >
              {saved ? <CheckIcon /> : <DownloadIcon />}
              <span className="mermaid-card-btn-text">{saved ? "Saved" : "Save SVG"}</span>
            </button>
          </div>
        </div>

        {/* Clean clickable diagram canvas with zero overlapping controls */}
        <div
          className="mermaid-card-viewport"
          onClick={() => setIsLightboxOpen(true)}
          title="Click diagram to expand, pan & zoom"
          dangerouslySetInnerHTML={{ __html: svgCode }}
        />

        {/* Subtle bottom hint bar */}
        <div className="mermaid-card-footer" onClick={() => setIsLightboxOpen(true)}>
          <span className="mermaid-card-hint">
            <ExpandIcon width={12} height={12} /> Click diagram to expand & zoom
          </span>
        </div>
      </div>

      {isLightboxOpen && (
        <MermaidLightboxModal
          svgCode={svgCode}
          chart={cleanChart}
          diagramType={diagramType}
          onClose={() => setIsLightboxOpen(false)}
        />
      )}
    </>
  );
}
