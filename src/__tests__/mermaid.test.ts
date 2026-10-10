// ── surface: shared ──────────────────────────────────
// Unit tests for Mermaid diagram utilities and export behaviors.
// Exports: (none — test module) · Related: components/markdown/MermaidDiagram.tsx

import { describe, it, expect, vi } from "vitest";
import {
  getDiagramType,
  getSvgDimensions,
  downloadSvg,
} from "../components/markdown/MermaidDiagram";

describe("getDiagramType", () => {
  it("identifies flowcharts and graphs", () => {
    expect(getDiagramType("graph TD\n  A --> B")).toBe("Flowchart");
    expect(getDiagramType("flowchart LR\n  Start --> Stop")).toBe("Flowchart");
  });

  it("identifies sequence diagrams", () => {
    expect(getDiagramType("sequenceDiagram\n  Alice->>Bob: Hello")).toBe("Sequence Diagram");
  });

  it("identifies class diagrams", () => {
    expect(getDiagramType("classDiagram\n  Animal <|-- Duck")).toBe("Class Diagram");
  });

  it("identifies state diagrams", () => {
    expect(getDiagramType("stateDiagram-v2\n  [*] --> Still")).toBe("State Diagram");
  });

  it("identifies ER diagrams", () => {
    expect(getDiagramType("erDiagram\n  CUSTOMER ||--o{ ORDER : places")).toBe("ER Diagram");
  });

  it("identifies gantt, pie, gitGraph, mindmap, timeline", () => {
    expect(getDiagramType("gantt\n  title A Gantt")).toBe("Gantt Chart");
    expect(getDiagramType("pie\n  title Key elements")).toBe("Pie Chart");
    expect(getDiagramType("gitGraph\n  commit")).toBe("Git Graph");
    expect(getDiagramType("mindmap\n  root((mindmap))")).toBe("Mindmap");
    expect(getDiagramType("timeline\n  title History")).toBe("Timeline");
  });

  it("ignores leading comments", () => {
    const chart = `%% This is a comment
%% Another comment
flowchart TD
  A --> B`;
    expect(getDiagramType(chart)).toBe("Flowchart");
  });

  it("falls back gracefully for unknown types or empty input", () => {
    expect(getDiagramType("")).toBe("Diagram");
    expect(getDiagramType("customDiagramType foo")).toBe("Mermaid Diagram");
  });
});

describe("getSvgDimensions", () => {
  it("extracts dimensions from viewBox", () => {
    const svg = `<svg viewBox="0 0 350 700" width="100%"><g/></svg>`;
    expect(getSvgDimensions(svg)).toEqual({ width: 350, height: 700 });
  });

  it("extracts dimensions from width and height attributes", () => {
    const svg = `<svg width="450px" height="250px"><g/></svg>`;
    expect(getSvgDimensions(svg)).toEqual({ width: 450, height: 250 });
  });

  it("falls back to default dimensions when none are present", () => {
    const svg = `<svg><g/></svg>`;
    expect(getSvgDimensions(svg)).toEqual({ width: 800, height: 600 });
  });
});

describe("downloadSvg", () => {
  it("creates a blob and triggers download on an anchor element", async () => {
    const createObjectURL = vi.fn(() => "blob:mock-url");
    const revokeObjectURL = vi.fn();
    window.URL.createObjectURL = createObjectURL;
    window.URL.revokeObjectURL = revokeObjectURL;

    const clickSpy = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => {});

    await downloadSvg("<svg><circle r='10'/></svg>", "test.svg");

    expect(createObjectURL).toHaveBeenCalledTimes(1);
    expect(clickSpy).toHaveBeenCalledTimes(1);
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:mock-url");

    clickSpy.mockRestore();
  });
});

