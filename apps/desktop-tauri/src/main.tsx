import React from "react";
import ReactDOM from "react-dom/client";
import { listen } from "@tauri-apps/api/event";
import App from "./App";
import "./styles/globals.css";
import "./styles/inline-thinking-dot.css";
import "./styles/inline-approval-card.css";

ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
);

// Remove the splash screen only after the backend health check passes
// (hermix:server-ready event from Tauri) OR after a 15s safety timeout.
function dismissSplash() {
  const splash = document.getElementById("splash");
  if (splash) {
    splash.classList.add("hide");
    setTimeout(() => splash.remove(), 400);
  }
}

// Wait for real backend readiness signal
let dismissed = false;
function safeDismiss() {
  if (dismissed) return;
  dismissed = true;
  dismissSplash();
}

listen("hermix:server-ready", () => {
  // Small delay so the "后端已就绪" message is visible briefly
  setTimeout(safeDismiss, 600);
});

// Safety timeout: if the event never fires (e.g. sidecar failed),
// dismiss the splash after 15s so the user isn't stuck.
setTimeout(safeDismiss, 15000);
