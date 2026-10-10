// ── surface: chat ──────────────────────────────────
// Turn-state container — messages/segments/pending/busy/error/edit state + images.
// Exports: useTurnRunner · Related: App.tsx (runTurn), Composer.tsx

import { useState, useCallback, useEffect } from "react";
import type { Message, Segment } from "../lib/provider";
import { imageFileToDataUrl, type PendingEdit } from "../utils";

export function useTurnRunner() {
  const [messages, setMessages] = useState<Message[]>([]);
  const [segments, setSegments] = useState<Segment[]>([]);
  const [pendingTool, setPendingTool] = useState<string | null>(null);
  const [pendingEdit, setPendingEdit] = useState<PendingEdit | null>(null);
  const [busy, setBusy] = useState<boolean>(false);
  // Stop was clicked AND the backend acknowledged a live turn to cancel
  // (stop_turn returned true). The composer shows a disabled "Stopping…"
  // button while this is set, so the click gets immediate feedback instead of
  // appearing dead until the turn's Done event arrives.
  const [isStopping, setIsStopping] = useState(false);
  // Single reset point for every way a turn can end — runTurn's finally,
  // compaction's own finally, and the early-error paths — keyed to the `busy`
  // edge instead of to runTurn's finally alone. Resetting only there would
  // latch a stop issued against COMPACTION (also `busy`, also shared via the
  // same stop()) for the entire next turn, with the Stop button disabled.
  useEffect(() => {
    if (!busy) setIsStopping(false);
  }, [busy]);
  const [error, setError] = useState<string | null>(null);
  const [editingIdx, setEditingIdx] = useState<number | null>(null);
  const [editDraft, setEditDraft] = useState("");
  const [copiedIdx, setCopiedIdx] = useState<number | null>(null);
  const [images, setImages] = useState<string[]>([]);

  async function addImageFiles(files: FileList | File[]) {
    const imgs = Array.from(files).filter((f) => f.type.startsWith("image/"));
    const urls = (
      await Promise.all(imgs.map((f) => imageFileToDataUrl(f)))
    ).filter(Boolean);
    if (urls.length) setImages((prev) => [...prev, ...urls]);
  }

  function copyMessage(content: string, i: number) {
    navigator.clipboard
      .writeText(content)
      .then(() => {
        setCopiedIdx(i);
        setTimeout(() => setCopiedIdx((cur) => (cur === i ? null : cur)), 1500);
      })
      .catch(() => {});
  }

  function startEdit(i: number) {
    if (busy) return;
    setEditingIdx(i);
    setEditDraft(messages[i].content);
  }

  function cancelEdit() {
    setEditingIdx(null);
    setEditDraft("");
  }

  const clearTurnState = useCallback(() => {
    setSegments([]);
    setPendingTool(null);
    setPendingEdit(null);
  }, []);

  return {
    messages,
    setMessages,
    segments,
    setSegments,
    pendingTool,
    setPendingTool,
    pendingEdit,
    setPendingEdit,
    busy,
    setBusy,
    isStopping,
    setIsStopping,
    error,
    setError,
    editingIdx,
    setEditingIdx,
    editDraft,
    setEditDraft,
    copiedIdx,
    setCopiedIdx,
    images,
    setImages,
    addImageFiles,
    copyMessage,
    startEdit,
    cancelEdit,
    clearTurnState,
  };
}

