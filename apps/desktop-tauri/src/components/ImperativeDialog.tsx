/**
 * Imperative dialog — renders once, stays in DOM, toggles via CSS.
 * Zero React re-render latency. Children handle their own backdrop/styling.
 */
import { useRef, useImperativeHandle, forwardRef, type ReactNode } from "react";

export interface ImperativeDialogHandle {
  open: () => void;
  close: () => void;
}

export const ImperativeDialog = forwardRef<
  ImperativeDialogHandle,
  { children: ReactNode }
>(function ImperativeDialog({ children }, ref) {
  const rootRef = useRef<HTMLDivElement>(null);

  useImperativeHandle(ref, () => ({
    open() {
      const r = rootRef.current;
      if (r) r.style.display = "";
    },
    close() {
      const r = rootRef.current;
      if (r) r.style.display = "none";
    },
  }));

  return (
    <div
      ref={rootRef}
      style={{
        display: "none",
        position: "fixed",
        inset: 0,
        zIndex: 2147483647,
        pointerEvents: "auto",
      }}
    >
      {children}
    </div>
  );
});
