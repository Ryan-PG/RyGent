import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { applyTheme, bootTheme } from "./settings/theme";
import "./styles.css";

/**
 * Paint the theme before React renders anything.
 *
 * The stored preference lives in SQLite and arrives over an async command, so at
 * this point the app does not yet know which palette to use - and rendering the
 * default first would show a full-window flash of the wrong one on every launch.
 *
 * `bootTheme()` answers it immediately from the `localStorage` cache, falling
 * back to the OS preference on a first launch. The store seeds its own
 * `resolvedTheme` from the same function, so the terminal starts on the palette
 * this line just painted.
 *
 * This is deliberately the *only* thing that runs before the app mounts, and it
 * is synchronous: anything awaited here would be too late to matter.
 */
applyTheme(bootTheme());

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
