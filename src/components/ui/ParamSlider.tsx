// ── surface: shared ──────────────────────────────────
// Slider input for sampling params.
// Exports: ParamSlider · Related: SettingsModal.tsx

export function ParamSlider({
  label,
  value,
  min,
  max,
  step,
  onChange,
  disabled,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  disabled?: boolean;
}) {
  return (
    <div className={`param-row${disabled ? " disabled" : ""}`}>
      <span className="param-label">{label}</span>
      <input
        className="param-slider"
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(parseFloat(e.target.value))}
        disabled={disabled}
      />
      <span className="param-value">{value.toFixed(2)}</span>
    </div>
  );
}

