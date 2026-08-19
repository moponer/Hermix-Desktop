/**
 * Global toast notification system.
 *
 * Provides a lightweight, singleton toast manager that any component
 * can call without prop drilling. Uses an external store so non-React
 * code (api.ts, hooks) can also trigger toasts.
 */

import { useEffect, useState, useCallback } from "react";

export interface ToastItem {
  id: number;
  message: string;
  variant: "error" | "warning" | "info" | "success";
  duration: number;
}

type Listener = (toasts: ToastItem[]) => void;

let toasts: ToastItem[] = [];
let listeners: Set<Listener> = new Set();
let nextId = 1;

function emit() {
  const snapshot = toasts;
  listeners.forEach((fn) => fn(snapshot));
}

/**
 * Per-variant default auto-dismiss duration. Errors stay on screen
 * longer (5s) because they demand attention; success disappears
 * quickly (2.5s) so it doesn't pile up during fast delete / rename
 * bursts. Pass an explicit `duration` to override.
 */
const DEFAULT_DURATION_MS: Record<ToastItem["variant"], number> = {
  success: 2500,
  info:    3000,
  warning: 4000,
  error:   5000,
};

/** Push a toast. Non-React code can call this directly. */
export function toast(
  message: string,
  variant: ToastItem["variant"] = "info",
  duration: number = DEFAULT_DURATION_MS[variant],
): void {
  const item: ToastItem = { id: nextId++, message, variant, duration };
  toasts = [...toasts, item];
  emit();
  if (duration > 0) {
    setTimeout(() => dismissToast(item.id), duration);
  }
}

/** Convenience helpers */
export const toastError = (msg: string) => toast(msg, "error");
export const toastWarning = (msg: string) => toast(msg, "warning");
export const toastInfo = (msg: string) => toast(msg, "info");
export const toastSuccess = (msg: string) => toast(msg, "success");

/** Remove a toast by id */
export function dismissToast(id: number): void {
  toasts = toasts.filter((t) => t.id !== id);
  emit();
}

/** Test-only: wipe the singleton toast stack and reset id counter.
 *  Without this, toasts pushed in one test bleed into the next. */
export function __resetToastsForTest(): void {
  // Drop any pending auto-dismiss timers by clearing the array.
  // The original timers will fire against ids that no longer exist
  // — dismissToast is a no-op filter, so this is safe.
  toasts = [];
  nextId = 1;
  emit();
}

/** Subscribe to toast changes. Returns an unsubscribe function. */
export function subscribeToasts(fn: Listener): () => void {
  listeners.add(fn);
  fn(toasts);
  return () => { listeners.delete(fn); };
}

/**
 * React hook for consuming toasts in a component.
 * Returns the current toast list and a dismiss handler.
 */
export function useToasts() {
  const [items, setItems] = useState<ToastItem[]>(toasts);

  useEffect(() => {
    return subscribeToasts(setItems);
  }, []);

  const dismiss = useCallback((id: number) => dismissToast(id), []);

  return { toasts: items, dismiss };
}
