// ── surface: shared ──────────────────────────────────
// Confirm/prompt boxes backed by App's confirmBox/promptBox.
// Exports: PromptModal, ModalState · Related: App.tsx, ApprovalOverlay.tsx

import React from "react";
import { useBackdropClose } from "../hooks/useBackdropClose";

export type ModalState =
  | { kind: "confirm"; title: string; message: string; resolve: (val: boolean) => void }
  | { kind: "prompt"; title: string; value: string; resolve: (val: string | null) => void }
  | null;

export function PromptModal({
  modal,
  setModal,
  okModal,
  cancelModal,
}: {
  modal: ModalState;
  setModal: React.Dispatch<React.SetStateAction<ModalState>>;
  okModal: () => void;
  cancelModal: () => void;
}) {
  const backdropHandlers = useBackdropClose(cancelModal);

  if (!modal) return null;

  return (
    <div className="approval-overlay" {...backdropHandlers}>
      <div
        className="prompt-modal"
        onClick={(e) => e.stopPropagation()}
        onMouseDown={(e) => e.stopPropagation()}
      >
        <div className="prompt-title">{modal.title}</div>
        {modal.kind === "confirm" ? (
          <div className="prompt-message">{modal.message}</div>
        ) : (
          <input
            className="prompt-input"
            autoFocus
            value={modal.value}
            onChange={(e) =>
              setModal((m) =>
                m && m.kind === "prompt"
                  ? { ...m, value: e.target.value }
                  : m,
              )
            }
            onKeyDown={(e) => {
              if (e.key === "Enter") okModal();
              else if (e.key === "Escape") cancelModal();
            }}
          />
        )}
        <div className="prompt-actions">
          <button className="prompt-ok" onClick={okModal}>
            OK
          </button>
          <button className="prompt-cancel" onClick={cancelModal}>
            Cancel
          </button>
        </div>
      </div>
    </div>
  );
}
