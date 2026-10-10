// ── surface: shared ──────────────────────────────────
// Prevents backdrop dismiss when a user drags to select text inside a dialog
// or input and accidentally releases the mouse outside on the overlay backdrop.
// Exports: useBackdropClose · Related: PromptModal.tsx, SettingsModal.tsx, ExportModal.tsx

import React, { useRef } from "react";

/**
 * Ensures `onClose` is only called if BOTH `mousedown` and `click` (mouseup)
 * occurred directly on the backdrop overlay itself (i.e. `e.target === e.currentTarget`),
 * and did NOT originate from text selection or mouse interaction inside the dialog.
 */
export function useBackdropClose(onClose?: () => void) {
  const isBackdropTarget = useRef(false);

  const onMouseDown = (e: React.MouseEvent) => {
    isBackdropTarget.current = e.target === e.currentTarget;
  };

  const onClick = (e: React.MouseEvent) => {
    if (isBackdropTarget.current && e.target === e.currentTarget) {
      onClose?.();
    }
    isBackdropTarget.current = false;
  };

  return { onMouseDown, onClick };
}
