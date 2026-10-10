// ── surface: shared ──────────────────────────────────
// Toggle-switch row (settings, tools popup, lite panel).
// Exports: Switch · Related: SettingsModal.tsx, ChatSurface.tsx

export function Switch({
  label,
  checked,
  onChange,
  disabled,
}: {
  label?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <label className={`switch-row${!label ? " switch-compact" : ""}${disabled ? " disabled" : ""}`}>
      {label && <span>{label}</span>}
      <input
        type="checkbox"
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange(e.target.checked)}
      />
    </label>
  );
}
