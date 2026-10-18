// ── surface: shared ──────────────────────────────────
// Model + sampling + tool-toggle state, seeded from prefs. Still state-only.
// Exports: useModelConfig · Related: App.tsx, utils.ts (Prefs)

import { useState } from "react";
import type { Prefs } from "../utils";

export function useModelConfig(prefs0: Prefs) {
  const [models, setModels] = useState<string[]>([]);
  const [model, setModel] = useState<string>(prefs0.model ?? "");
  const [keyStatus, setKeyStatus] = useState<Record<string, boolean>>({});

  const [samplingOn, setSamplingOn] = useState<boolean>(
    prefs0.samplingOn ?? false,
  );
  const [temperature, setTemperature] = useState<number>(
    prefs0.temperature ?? 1.0,
  );
  const [topP, setTopP] = useState<number>(prefs0.topP ?? 0.95);
  const [maxTokens, setMaxTokens] = useState<string>(prefs0.maxTokens ?? "");
  const [thinkingEffort, setThinkingEffort] = useState<string>(
    prefs0.thinkingEffort ?? "high",
  );
  // Empty = no cap (use the model's full window). Set it to enable the usage
  // nudge and the "assembled context" tail line — both are cap-driven.
  const [maxContext, setMaxContext] = useState<string>(
    prefs0.maxContext ?? "",
  );
  const [maxToolCalls, setMaxToolCalls] = useState<string>(
    prefs0.maxToolCalls ?? "",
  );

  const [mdWriteOn, setMdWriteOn] = useState<boolean>(prefs0.mdWriteOn ?? true);
  const [notesOn, setNotesOn] = useState<boolean>(prefs0.notesOn ?? true);
  const [htmlWriteOn, setHtmlWriteOn] = useState<boolean>(
    prefs0.htmlWriteOn ?? false,
  );
  const [codeOn, setCodeOn] = useState<boolean>(prefs0.codeOn ?? false);

  return {
    models,
    setModels,
    model,
    setModel,
    keyStatus,
    setKeyStatus,
    samplingOn,
    setSamplingOn,
    temperature,
    setTemperature,
    topP,
    setTopP,
    maxTokens,
    setMaxTokens,
    thinkingEffort,
    setThinkingEffort,
    maxContext,
    setMaxContext,
    maxToolCalls,
    setMaxToolCalls,
    mdWriteOn,
    setMdWriteOn,
    notesOn,
    setNotesOn,
    htmlWriteOn,
    setHtmlWriteOn,
    codeOn,
    setCodeOn,
  };
}
