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
import { isCircuitTripped } from "../api";

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
    // Skip when the circuit breaker is tripped — the backend is unreachable
    // and polling would only waste resources.
    if (isCircuitTripped()) {
      if (showSpinner) setLoading(false);
      return;
    }
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

  // Initial load + auto-refresh polling
  useEffect(() => {
    mountedRef.current = true;
    consecutiveFailuresRef.current = 0;
    loadApps();

    // Poll every N seconds so apps created via the LLM tool appear
    // without the user needing to click the refresh button. Stop after
    // MAX_CONSECUTIVE_FAILURES to avoid hammering a down backend.
    const id = setInterval(() => {
      if (consecutiveFailuresRef.current >= MAX_CONSECUTIVE_FAILURES) return;
      if (isCircuitTripped()) return;
      loadApps(false);
    }, AUTO_REFRESH_INTERVAL_MS);
    return () => {
      mountedRef.current = false;
      clearInterval(id);
    };
  }, [loadApps]);

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
    if (!confirm("确定要删除此应用吗？此操作不可撤销。")) return;
    setError(null);
    try {
      await deleteCustomApp(id);
      setSuccessMsg("应用已删除");
      await loadApps();
      safeSetTimeout(() => setSuccessMsg(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
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
      <div className="custom-apps-view__header">
        <div className="custom-apps-view__title-row">
          <h1 className="custom-apps-view__title">
            <Boxes size={24} />
            自定义应用
          </h1>
          <button
            onClick={onStartCreateApp}
            className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[13px] font-medium transition-colors hover:opacity-90"
            style={{
              marginLeft: "auto",
              marginRight: 8,
              background: "var(--color-accent, #b8860b)",
              color: "#fff",
            }}
            title="创建新应用"
          >
            <Plus size={16} />
            创建新应用
          </button>
          <button
            className="custom-apps-view__refresh"
            onClick={() => loadApps()}
            disabled={loading}
            title="刷新"
          >
            <RefreshCw size={16} className={loading ? "spin" : ""} />
          </button>
        </div>
        <p className="custom-apps-view__subtitle">
          管理通过对话创建的桌面应用 · 共 {apps.length} 个 · 已打包 {packagedCount} 个
        </p>
      </div>

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
          <Search size={16} />
          <input
            type="text"
            placeholder="搜索应用..."
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </div>
      </div>

      <div className="custom-apps-view__content">
        {loading && apps.length === 0 ? (
          <div className="custom-apps-view__empty">
            <div className="custom-apps-view__empty-spinner" />
            <p>加载中...</p>
          </div>
        ) : filteredApps.length === 0 ? (
          <div className="custom-apps-view__empty">
            <Boxes size={48} className="custom-apps-view__empty-icon" />
            <h3 className="custom-apps-view__empty-title">
              {query.trim() ? "没有匹配的应用" : "还没有自定义应用"}
            </h3>
            <p className="custom-apps-view__empty-desc">
              {query.trim()
                ? "尝试其他搜索关键词"
                : "在对话中让 AI 为你创建一个桌面应用，它将出现在这里。"}
            </p>
          </div>
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
                onDelete={() => handleDelete(app.id)}
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
    </div>
  );
}
