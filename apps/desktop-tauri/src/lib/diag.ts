// Dev-only diagnostic logging. Always on under `import.meta.env.DEV`
// (vite dead-code-eliminates the branch in production); in production
// builds opt-in via `localStorage.setItem("hermix.diag", "1")` — gated
// by a runtime check so call sites cost nothing when disabled.

function isDiagEnabled(): boolean {
  if (import.meta.env.DEV) return true;
  if (typeof window === "undefined") return false;
  try {
    return window.localStorage.getItem("hermix.diag") === "1";
  } catch {
    return false;
  }
}

export function diagLog(...args: unknown[]): void {
  if (isDiagEnabled()) {
    // eslint-disable-next-line no-console
    console.log(...args);
  }
}

export function diagWarn(...args: unknown[]): void {
  if (isDiagEnabled()) {
    // eslint-disable-next-line no-console
    console.warn(...args);
  }
}

// Monotonic id for correlating one send across the diag logs.
export function nextDiagId(): number {
  const g = globalThis as unknown as { __sendDiagId?: number };
  g.__sendDiagId = (g.__sendDiagId || 0) + 1;
  return g.__sendDiagId;
}

export function currentDiagId(): number {
  const g = globalThis as unknown as { __sendDiagId?: number };
  return g.__sendDiagId || 0;
}
