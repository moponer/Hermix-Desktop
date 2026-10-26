// CustomAppsView — the main view for browsing and managing custom apps

import { useState, useEffect, useCallback, useRef } from "react";
import { RefreshCw, Search, Boxes, Plus } from "lucide-react";
import type { CustomAppSummary } from "../lib/customAppApi";
import {
  listCustomApps,
  packageCustomApp,
  runCustomApp,
  deleteCustomApp,
  openCustomAppDist,
} from "../lib/customAppApi";
import { AppCard } from "./custom-apps/AppCard";
import { AppDetailModal } from "./custom-apps/AppDetailModal";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { usePolling } from "../hooks/usePolling";
import { EmptyState, PageHeader, SkeletonCard, ConfirmDialog } from "./ui";

/** Auto-refresh interval (ms) — keeps the list in sync when apps are
 *  created/updated/deleted via the LLM tool while this page is open. */
const AUTO_REFRESH_INTERVAL_MS = 5_000;
/** Stop polling after this many consecutive failures — prevents the
 *  5-second timer from hammering a backend that's clearly down. The
 *  user can still click the refresh button to re-probe manually. */
const MAX_CONSECUTIVE_FAILURES = 3;

export function CustomAppsView({ onStartCreateApp }: { onStartCreateApp?: () => void }) {
  const safeSetTimeout = useSafeTimeout();
  const [apps, setApps] = useState<CustomAppSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [selectedAppId, setSelectedAppId] = useState<string | null>(null);
  const [packagingId, setPackagingId] = useState<string | null>(null);
  const [successMsg, setSuccessMsg] = useState<string | null>(null);
  const mountedRef = useRef(true);
  const consecutiveFailuresRef = useRef(0);

  const loadApps = useCallback(async (showSpinner = true) => {
    if (showSpinner) setLoading(true);
    setError(null);
    try {
      const list = await listCustomApps();
      consecutiveFailuresRef.current = 0;
      if (mountedRef.current) setApps(list);
    } catch (e) {
      consecutiveFailuresRef.current++;
      if (mountedRef.current) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (mountedRef.current && showSpinner) setLoading(false);
    }
  }, []);

  // 首次加载（带 spinner）。自动刷新交给 usePolling（P2-4：隐藏页不发请求、
  // 恢复可见立即补一次）。
  useEffect(() => {
    mountedRef.current = true;
    consecutiveFailuresRef.current = 0;
    void loadApps();
    return () => {
      mountedRef.current = false;
    };
  }, [loadApps]);

  usePolling(
    () => {
      // 连续失败达阈值就停：后端明显挂了，别继续打。
      if (consecutiveFailuresRef.current >= MAX_CONSECUTIVE_FAILURES) return;
      void loadApps(false);
    },
    AUTO_REFRESH_INTERVAL_MS,
    { immediate: false },
  );

  // Refresh when the window regains focus (user switches back from
  // the chat tab where the LLM just created an app).
  useEffect(() => {
    const onFocus = () => loadApps(false);
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [loadApps]);

  const handlePackage = async (id: string) => {
    setPackagingId(id);
    setError(null);
    try {
      // hx-shell 架构统一用 sidecar 模式（esbuild 编译 + 单文件尾部注入）
      const result = await packageCustomApp(id, "sidecar");
      setSuccessMsg(result.message);
      await loadApps();
      safeSetTimeout(() => setSuccessMsg(null), 3000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPackagingId(null);
    }
  };

  const handleOpenDist = async (path: string) => {
    setError(null);
    try {
      await openCustomAppDist(path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleRun = async (id: string) => {
    setError(null);
    try {
      await runCustomApp(id);
      setSuccessMsg("应用已启动");
      safeSetTimeout(() => setSuccessMsg(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const handleDelete = async (id: string) => {
    setError(null);
    setSuccessMsg(null);
    try {
      const app = apps.find((a) => a.id === id);
      await deleteCustomApp(id);
      setSuccessMsg(`已删除「${app?.name ?? id}」→ 回收站（%TEMP%/hermix-trash/），可恢复`);
      await loadApps();
      safeSetTimeout(() => setSuccessMsg(null), 4000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  // Deleting a custom app is destructive — show a confirmation modal
  // rather than the native browser confirm(). The backend mv's the
  // app to %TEMP%/hermix-trash/<id>-<ts> so the user can recover.
  const [deleteTarget, setDeleteTarget] = useState<{ id: string; name: string } | null>(null);
  const requestDelete = (id: string, name: string) => setDeleteTarget({ id, name });
  const cancelDelete = () => setDeleteTarget(null);
  const confirmDelete = async () => {
    if (!deleteTarget) return;
    const { id } = deleteTarget;
    setDeleteTarget(null);
    await handleDelete(id);
  };

  const filteredApps = apps.filter((app) => {
    if (!query.trim()) return true;
    const q = query.toLowerCase();
    return (
      app.name.toLowerCase().includes(q) ||
      app.description.toLowerCase().includes(q) ||
      app.category.toLowerCase().includes(q) ||
      app.id.toLowerCase().includes(q)
    );
  });

  const packagedCount = apps.filter((a) => a.is_packaged).length;

  return (
    <div className="custom-apps-view">
      <PageHeader
        icon={<Boxes className="icon-lg" />}
        title="自定义应用"
        count={`${apps.length} 个 · 已打包 ${packagedCount}`}
        actions={
          <>
            <button
              onClick={onStartCreateApp}
              className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[length:var(--text-ui)] font-medium transition-colors hover:opacity-90 bg-accent text-on-accent"
              title="创建新应用"
            >
              <Plus className="icon-lg" />
              创建新应用
            </button>
            <button
              className="custom-apps-view__refresh"
              onClick={() => loadApps()}
              disabled={loading}
              title="刷新"
              aria-label="刷新应用列表"
            >
              <RefreshCw className={`icon-lg ${loading ? "spin" : ""}`} />
            </button>
          </>
        }
      />
      <p className="custom-apps-view__subtitle">
        管理通过对话创建的桌面应用
      </p>

      {error && (
        <div className="custom-apps-view__alert custom-apps-view__alert--error">
          {error}
        </div>
      )}
      {successMsg && (
        <div className="custom-apps-view__alert custom-apps-view__alert--success">
          {successMsg}
        </div>
      )}

      <div className="custom-apps-view__toolbar">
        <div className="custom-apps-view__search">
          <Search className="icon-lg" />
          <input
            type="text"
            placeholder="搜索应用…"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
      </div>

      <div className="custom-apps-view__content">
        {loading && apps.length === 0 ? (
          <div className="custom-apps-view__empty">
            <SkeletonCard lines={3} />
          </div>
        ) : filteredApps.length === 0 ? (
          <EmptyState
            variant="plain"
            icon={<Boxes className="icon-lg" />}
            title={query.trim() ? "没有匹配的应用" : "还没有自定义应用"}
            description={
              query.trim()
                ? "尝试其他搜索关键词"
                : "在对话中让 AI 为你创建一个桌面应用，它将出现在这里。"
            }
          />
        ) : (
          <div className="custom-apps-view__grid">
            {filteredApps.map((app) => (
              <AppCard
                key={app.id}
                app={app}
                onView={() => setSelectedAppId(app.id)}
                onPackage={() => handlePackage(app.id)}
                onOpenDist={() => app.dist_path && handleOpenDist(app.dist_path)}
                onRun={() => handleRun(app.id)}
                onDelete={() => requestDelete(app.id, app.name)}
                packaging={packagingId === app.id}
              />
            ))}
          </div>
        )}
      </div>

      {selectedAppId && (
        <AppDetailModal
          appId={selectedAppId}
          onClose={() => setSelectedAppId(null)}
          onChanged={loadApps}
        />
      )}

      {/* 删除确认：破坏性动作的唯一模式是 ui/ConfirmDialog（宪法 §九）。
          此前是手写弹窗：自带 hex 兜底色（var(--color-error, #dc2626)）与
          text-white 硬色，且无焦点陷阱 / 无 Esc / 无焦点归还。 */}
      <ConfirmDialog
        open={deleteTarget !== null}
        scrim
        title="删除应用"
        confirmLabel="移到回收站"
        body={
          deleteTarget ? (
            <>
              <p className="text-[length:var(--text-ui)] text-text-secondary leading-relaxed">
                确定要删除「<span className="font-medium text-text-primary">{deleteTarget.name}</span>」吗？
              </p>
              <p className="mt-2 text-[length:var(--text-meta)] text-text-muted leading-relaxed">
                此操作会从 <code className="font-mono text-[length:var(--text-meta)] px-1 py-0.5 rounded">~/.hermix/custom-apps/{deleteTarget.id}/</code> 移到
                <code className="font-mono text-[length:var(--text-meta)] px-1 py-0.5 rounded ml-1">%TEMP%/hermix-trash/{deleteTarget.id}-&lt;ts&gt;</code>。
                <br />后悔可在回收站里恢复。
              </p>
            </>
          ) : null
        }
        onCancel={cancelDelete}
        onConfirm={() => void confirmDelete()}
      />
    </div>
  );
}
