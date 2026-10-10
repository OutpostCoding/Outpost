// ── surface: shared ──────────────────────────────────
// React entry point — mounts <App/>.
// Exports: none (side effect) · Related: App.tsx

import React from "react";
import ReactDOM from "react-dom/client";
import "@fontsource-variable/source-serif-4";
import App from "./App";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
