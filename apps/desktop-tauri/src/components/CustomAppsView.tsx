// CustomAppsView — the main view for browsing and managing custom apps

import { useState, useEffect, useCallback } from "react";
import { RefreshCw, Search, Boxes } from "lucide-react";
import type { CustomAppSummary } from "../lib/customAppApi";
import {
  listCustomApps,
  packageCustomApp,
  runCustomApp,
  deleteCustomApp,
} from "../lib/customAppApi";
import { AppCard } from "./custom-apps/AppCard";
import { AppDetailModal } from "./custom-apps/AppDetailModal";

export function CustomAppsView() {
  const [apps, setApps] = useState<CustomAppSummary[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [selectedAppId, setSelectedAppId] = useState<string | null>(null);
  const [packagingId, setPackagingId] = useState<string | null>(null);
  const [successMsg, setSuccessMsg] = useState<string | null>(null);

  const loadApps = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const list = await listCustomApps();
      setApps(list);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadApps();
  }, [loadApps]);

  const handlePackage = async (id: string) => {
    setPackagingId(id);
    setError(null);
    try {
      const result = await packageCustomApp(id);
      setSuccessMsg(result.message);
      await loadApps();
      setTimeout(() => setSuccessMsg(null), 3000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPackagingId(null);
    }
  };

  const handleRun = async (id: string) => {
    setError(null);
    try {
      await runCustomApp(id);
      setSuccessMsg("应用已启动");
      setTimeout(() => setSuccessMsg(null), 2000);
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
      setTimeout(() => setSuccessMsg(null), 2000);
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
            className="custom-apps-view__refresh"
            onClick={loadApps}
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
