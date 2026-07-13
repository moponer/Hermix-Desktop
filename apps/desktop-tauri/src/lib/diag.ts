/**
 * diag.ts — dev-only diagnostic logging.
 *
 * The streaming pipeline and message queue emit a lot of low-level
 * diagnostics (send dedup, WS connect/retry, queue drain). These are
 * invaluable while debugging but must NOT spam a production user's
 * DevTools console.
 *
 * Gate rule (mirrors the storageCompat migration flag):
 *   - ALWAYS on in `import.meta.env.DEV` (vite dead-code-eliminates the
 *     `if` in a production build, so the call sites cost nothing).
 *   - In a production build, only when the user explicitly opts in via
 *     `localStorage.setItem("hermix.diag", "1")` (surfaced from Settings
 *     → Advanced, or typed in console).
 *
 * Use `diagLog` / `diagWarn` exactly like `console.log` / `console.warn`.
 */

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

/** Monotonic id for correlating a single send across the diag logs. */
export function nextDiagId(): number {
  const g = globalThis as unknown as { __sendDiagId?: number };
  g.__sendDiagId = (g.__sendDiagId || 0) + 1;
  return g.__sendDiagId;
}

export function currentDiagId(): number {
  const g = globalThis as unknown as { __sendDiagId?: number };
  return g.__sendDiagId || 0;
}
