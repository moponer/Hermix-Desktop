// 全屏产物预览模态框，从 App.tsx 抽出的内部子组件。
// 挂载在 App 根部，覆盖侧边栏/标题栏，仅通过 X / ESC / 点击背景关闭。
import { useEffect } from "react";
import { PreviewPanel } from "../components/RightSidebar";
import type { Artifact } from "../utils/artifacts";

export function ArtifactPreviewModal({
  artifact,
  workspaceRoot,
  onClose,
}: {
  artifact: Artifact;
  workspaceRoot?: string | null;
  onClose: () => void;
}) {
  // ESC closes the modal. Effect attaches the listener only while
  // the modal is mounted; cleanup removes it on unmount so we don't
  // leak handlers when the user navigates to other views.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.stopPropagation();
        onClose();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div
      className="fixed inset-0 z-[60] flex items-stretch justify-center"
      // Backdrop click closes the modal. We deliberately use a
      // direct `onClick` (not a stopPropagation wrapper) — child
      // clicks bubble up through the panel but we check `e.target
      // === e.currentTarget` so panel-internal clicks don't
      // accidentally close it.
      onClick={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      // Two-layer backdrop: a 65% black scrim for contrast + a soft
      // 2px blur of the underlying app, so the dialog reads as a
      // genuine "floating" surface rather than a hard pop. The blur
      // is small on purpose — the desktop UI behind is the user's
      // own chat, over-blurring it would feel like the app froze.
      style={{
        background: "var(--color-overlay-scrim)",
        backdropFilter: "blur(2px)",
        WebkitBackdropFilter: "blur(2px)",
      }}
    >
      <div
        // Modal box — premium frame:
        // • width: 1100px cap (92vw on narrow screens)
        // • height: 88vh so the desktop titlebar is still visible
        // for drag/resize affordance
        // • 16px radius (xl) + 1px border + 2px outline (subtle
        // light-mode highlight) + heavy shadow
        // • background uses theme bg-primary (auto light/dark)
        // • outline gives the panel a hairline highlight that
        // survives any bg color, so light mode reads "lifted off"
        // not "floating void"
        className="flex flex-col shadow-2xl
                   w-[min(1100px,92vw)] h-[88vh] mt-[5vh] mb-[5vh]
                   rounded-2xl overflow-hidden
                   border border-[color:var(--color-border-subtle)]
                   outline outline-2 outline-[color:color-mix(in_srgb,var(--color-text-primary)_6%,transparent)]
                   outline-offset-[-2px]
                   animate-[artifact-modal-in_240ms_ease-out]"
        style={{
          background: "var(--color-bg-primary)",
          color: "var(--color-text-primary)",
          boxShadow:
            "var(--shadow-modal)",
        }}
      >
        <PreviewPanel
          artifact={artifact}
          workspaceRoot={workspaceRoot}
          onBack={onClose}
        />
      </div>
    </div>
  );
}
