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

/** Push a toast. Non-React code can call this directly. */
export function toast(
  message: string,
  variant: ToastItem["variant"] = "info",
  duration = 4000,
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
