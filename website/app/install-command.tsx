"use client";

import { useState } from "react";
import { site } from "./site";

export default function InstallCommand() {
  const [status, setStatus] = useState("");

  async function copy() {
    try {
      await navigator.clipboard.writeText(site.install);
      setStatus("copied");
    } catch {
      setStatus("Select the command to copy it manually.");
    }
  }

  return (
    <div className="install">
      <code>{site.install}</code>
      <button type="button" onClick={copy} aria-label="Copy install command" title="Copy install command">
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" aria-hidden="true">
          {status === "copied" ? <path d="m5 12 4 4L19 6" /> : <>
            <rect x="8" y="8" width="12" height="12" rx="2" />
            <path d="M16 8V6a2 2 0 0 0-2-2H6a2 2 0 0 0-2 2v8a2 2 0 0 0 2 2h2" />
          </>}
        </svg>
      </button>
      <span className={status === "copied" ? "sr-only" : "status"} role="status">{status === "copied" ? "Install command copied" : status}</span>
    </div>
  );
}
