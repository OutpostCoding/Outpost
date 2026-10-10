// ── surface: shared ──────────────────────────────────
// Regression suite for the inline approval card's global keyboard shortcuts.
// The card advertises "press Enter to approve" / "Esc to reject", but is not
// focusable — the bindings must work from anywhere (window) while never
// hijacking typing in a field or double-firing a focused control.
// Exports: (none — test module) · Related: components/approval/InlineApprovalCard.tsx

import { describe, it, expect, afterEach, vi } from "vitest";
import { render, fireEvent, screen, cleanup } from "@testing-library/react";
import { InlineApprovalCard } from "../components/approval/InlineApprovalCard";
import type { PendingEdit } from "../utils";

// globals are off in vitest.config, so cleanup is manual (same reason as the
// other suites — RTL auto-cleanup needs globals).
afterEach(cleanup);

const card: PendingEdit = {
  id: 1,
  tool: "write_file",
  path: "src/main.rs",
  diff: "+hello",
};

describe("InlineApprovalCard keyboard shortcuts", () => {
  it("approves on Enter when no field holds focus", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    fireEvent.keyDown(window, { key: "Enter" });
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("approve");
  });

  it("rejects on Escape when no field holds focus", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    fireEvent.keyDown(window, { key: "Escape" });
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("reject", undefined);
  });

  it("does not hijack Enter typed into another field (e.g. the composer)", () => {
    const onAnswer = vi.fn();
    render(
      <>
        <textarea data-testid="composer" />
        <InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />
      </>,
    );
    const field = screen.getByTestId("composer") as HTMLTextAreaElement;
    field.focus();
    fireEvent.keyDown(window, { key: "Enter" });
    expect(onAnswer).not.toHaveBeenCalled();
  });

  it("takes focus on mount, so Enter approves without a click", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    const region = screen.getByRole("region", { name: "Tool approval required" });
    expect(document.activeElement).toBe(region);
    fireEvent.keyDown(window, { key: "Enter" });
    expect(onAnswer).toHaveBeenCalledWith("approve");
  });

  it("restores the previous focus when the card clears", () => {
    function Wrapper({ show }: { show: boolean }) {
      return (
        <>
          <textarea data-testid="composer" />
          {show && <InlineApprovalCard pendingEdit={card} onAnswer={vi.fn()} />}
        </>
      );
    }
    const { rerender } = render(<Wrapper show={false} />);
    const composer = screen.getByTestId("composer") as HTMLTextAreaElement;
    composer.focus();
    // The card appears (after a keyboard send the composer still holds focus) and
    // takes it, so the next Enter reaches the card's global binding.
    rerender(<Wrapper show={true} />);
    expect(document.activeElement).not.toBe(composer);
    // …and hands it back once the approval is resolved.
    rerender(<Wrapper show={false} />);
    expect(document.activeElement).toBe(composer);
  });
});

describe("InlineApprovalCard 'Always allow' visibility", () => {
  const commandCard = (remember: string[]): PendingEdit => ({
    id: 2,
    tool: "run_command",
    path: "C:/proj",
    diff: "$ git status",
    command: { text: "git status", remember },
  });

  it("offers Always allow when the backend found a pattern to persist", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={commandCard(["git"])} onAnswer={onAnswer} />);
    const btn = screen.getByRole("button", { name: "Always allow git" }) as HTMLButtonElement;
    expect(btn.disabled).toBe(false);
    fireEvent.click(btn);
    expect(onAnswer).toHaveBeenCalledWith("remember");
  });

  it("hides Always allow when the command can never auto-approve", () => {
    // `remember` is empty for a command that fails a guard (chaining, a pipe or
    // redirect, an escaping path). Showing a dead button — worse, one labelled with
    // the command's first word — read as "this is what it would add" when adding it
    // would do nothing. It is omitted instead.
    render(<InlineApprovalCard pendingEdit={commandCard([])} onAnswer={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /Always allow/ })).toBeNull();
  });

  it("does not offer Always allow on a non-command approval", () => {
    render(<InlineApprovalCard pendingEdit={card} onAnswer={vi.fn()} />);
    expect(screen.queryByRole("button", { name: /Always allow/ })).toBeNull();
  });
});

describe("InlineApprovalCard rejection feedback & warnings", () => {
  it("submits rejection with feedback when Enter is pressed in feedback input with text", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    const input = screen.getByPlaceholderText(/Feedback for model on reject/);
    input.focus();
    fireEvent.change(input, { target: { value: "Please use mock data instead" } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("reject", "Please use mock data instead");
  });

  it("approves on Enter in feedback input when input is empty or whitespace", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    const input = screen.getByPlaceholderText(/Feedback for model on reject/);
    input.focus();
    fireEvent.change(input, { target: { value: "   " } });
    fireEvent.keyDown(input, { key: "Enter" });
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("approve");
  });

  it("rejects on Escape in feedback input", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    const input = screen.getByPlaceholderText(/Feedback for model on reject/);
    input.focus();
    fireEvent.keyDown(input, { key: "Escape" });
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("reject");
  });

  it("updates Reject button label to 'Reject with note' and sends feedback on click", () => {
    const onAnswer = vi.fn();
    render(<InlineApprovalCard pendingEdit={card} onAnswer={onAnswer} />);
    const input = screen.getByPlaceholderText(/Feedback for model on reject/);
    fireEvent.change(input, { target: { value: "Do not touch config" } });
    const rejectBtn = screen.getByRole("button", { name: "Reject with note" });
    fireEvent.click(rejectBtn);
    expect(onAnswer).toHaveBeenCalledTimes(1);
    expect(onAnswer).toHaveBeenCalledWith("reject", "Do not touch config");
  });

  it("renders warning banner when warning text is present", () => {
    const warnCard: PendingEdit = {
      ...card,
      warning: "File was modified on disk since it was read",
    };
    render(<InlineApprovalCard pendingEdit={warnCard} onAnswer={vi.fn()} />);
    expect(
      screen.getByText("File was modified on disk since it was read"),
    ).toBeDefined();
  });
});

