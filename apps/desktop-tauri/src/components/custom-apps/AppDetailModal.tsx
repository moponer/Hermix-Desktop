// AppDetailModal — shows full app details including source code

import { useState, useEffect } from "react";
import { X, Package, Play, RefreshCw, Code2, FileText } from "lucide-react";
import type { CustomAppResponse } from "../../lib/customAppApi";
import { getCustomApp, packageCustomApp, runCustomApp, updateCustomApp } from "../../lib/customAppApi";

interface AppDetailModalProps {
  appId: string;
  onClose: () => void;
  onChanged: () => void;
}

type Tab = "info" | "backend" | "ui";

export function AppDetailModal({ appId, onClose, onChanged }: AppDetailModalProps) {
  const [app, setApp] = useState<CustomAppResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<Tab>("info");
  const [editing, setEditing] = useState(false);
  const [backendCode, setBackendCode] = useState("");
  const [uiCode, setUiCode] = useState("");
  const [saving, setSaving] = useState(false);
  const [packaging, setPackaging] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  useEffect(() => {
    loadApp();
  }, [appId]);

  const loadApp = async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await getCustomApp(appId);
      setApp(data);
      setBackendCode(data.backend_code);
      setUiCode(data.ui_code ?? "");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  const handleSave = async () => {
    if (!app) return;
    setSaving(true);
    setError(null);
    try {
      const updated = await updateCustomApp(appId, {
        backend_code: backendCode !== app.backend_code ? backendCode : undefined,
        ui_code: uiCode !== (app.ui_code ?? "") ? uiCode : undefined,
      });
      setApp(updated);
      setEditing(false);
      setSuccess("保存成功");
      onChanged();
      setTimeout(() => setSuccess(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSaving(false);
    }
  };

  const handlePackage = async () => {
    setPackaging(true);
    setError(null);
    try {
      const result = await packageCustomApp(appId);
      setSuccess(result.message);
      await loadApp();
      onChanged();
      setTimeout(() => setSuccess(null), 3000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setPackaging(false);
    }
  };

  const handleRun = async () => {
    setError(null);
    try {
      await runCustomApp(appId);
      setSuccess("应用已启动");
      setTimeout(() => setSuccess(null), 2000);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  if (loading) {
    return (
      <div className="modal-overlay" onClick={onClose}>
        <div className="modal-content modal-content--wide" onClick={(e) => e.stopPropagation()}>
          <div className="modal-loading">加载中...</div>
        </div>
      </div>
    );
  }

  if (!app) {
    return (
      <div className="modal-overlay" onClick={onClose}>
        <div className="modal-content modal-content--wide" onClick={(e) => e.stopPropagation()}>
          <div className="modal-error">应用不存在</div>
        </div>
      </div>
    );
  }

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-content modal-content--wide" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <div className="modal-header__title">
            {app.manifest.icon_file ? (
              <img
                className="modal-header__icon-img"
                src={`/api/custom-apps/${encodeURIComponent(appId)}/icon`}
                alt={app.manifest.name}
                draggable={false}
              />
            ) : (
              <span className="modal-header__icon">{app.manifest.icon}</span>
            )}
            <h2>{app.manifest.name}</h2>
            <span className="modal-header__version">v{app.manifest.version}</span>
            {app.is_packaged && (
              <span className="modal-header__badge">已打包</span>
            )}
          </div>
          <button className="modal-close" onClick={onClose}>
            <X size={18} />
          </button>
        </div>

        <div className="modal-tabs">
          <button
            className={`modal-tab ${tab === "info" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("info")}
          >
            <FileText size={14} /> 信息
          </button>
          <button
            className={`modal-tab ${tab === "backend" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("backend")}
          >
            <Code2 size={14} /> backend.ts
          </button>
          <button
            className={`modal-tab ${tab === "ui" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("ui")}
          >
            <Code2 size={14} /> ui.tsx
          </button>
        </div>

        <div className="modal-body">
          {error && (
            <div className="modal-alert modal-alert--error">{error}</div>
          )}
          {success && (
            <div className="modal-alert modal-alert--success">{success}</div>
          )}

          {tab === "info" && (
            <div className="app-detail-info">
              <div className="app-detail-info__row">
                <span className="app-detail-info__label">ID</span>
                <code className="app-detail-info__value">{app.manifest.id}</code>
              </div>
              <div className="app-detail-info__row">
                <span className="app-detail-info__label">描述</span>
                <span className="app-detail-info__value">{app.manifest.description}</span>
              </div>
              {app.manifest.long_description && (
                <div className="app-detail-info__row">
                  <span className="app-detail-info__label">详细描述</span>
                  <span className="app-detail-info__value">{app.manifest.long_description}</span>
                </div>
              )}
              <div className="app-detail-info__row">
                <span className="app-detail-info__label">作者</span>
                <span className="app-detail-info__value">{app.manifest.author}</span>
              </div>
              {app.manifest.icon_file && (
                <div className="app-detail-info__row">
                  <span className="app-detail-info__label">自定义图标</span>
                  <span className="app-detail-info__value">
                    <img
                      className="app-detail-info__icon-preview"
                      src={`/api/custom-apps/${encodeURIComponent(appId)}/icon`}
                      alt="icon"
                      draggable={false}
                    />
                    {app.manifest.icon_file}
                  </span>
                </div>
              )}
              <div className="app-detail-info__row">
                <span className="app-detail-info__label">分类</span>
                <span className="app-detail-info__value">{app.manifest.category}</span>
              </div>
              <div className="app-detail-info__row">
                <span className="app-detail-info__label">权限</span>
                <div className="app-detail-info__tags">
                  {app.manifest.permissions.length > 0 ? (
                    app.manifest.permissions.map((p) => (
                      <span key={p} className="app-detail-info__tag">{p}</span>
                    ))
                  ) : (
                    <span className="app-detail-info__value">无</span>
                  )}
                </div>
              </div>
              <div className="app-detail-info__row">
                <span className="app-detail-info__label">窗口</span>
                <span className="app-detail-info__value">
                  {app.manifest.window_config.width} × {app.manifest.window_config.height}
                  {app.manifest.window_config.resizable ? " (可调整)" : " (固定)"}
                </span>
              </div>
              {app.dist_path && (
                <div className="app-detail-info__row">
                  <span className="app-detail-info__label">输出目录</span>
                  <code className="app-detail-info__value">{app.dist_path}</code>
                </div>
              )}
            </div>
          )}

          {tab === "backend" && (
            <div className="app-detail-code">
              {editing ? (
                <textarea
                  className="app-detail-code__editor"
                  value={backendCode}
                  onChange={(e) => setBackendCode(e.target.value)}
                  spellCheck={false}
                />
              ) : (
                <pre className="app-detail-code__view">
                  <code>{backendCode}</code>
                </pre>
              )}
            </div>
          )}

          {tab === "ui" && (
            <div className="app-detail-code">
              {editing ? (
                <textarea
                  className="app-detail-code__editor"
                  value={uiCode}
                  onChange={(e) => setUiCode(e.target.value)}
                  spellCheck={false}
                  placeholder="此应用没有 UI 代码"
                />
              ) : (
                <pre className="app-detail-code__view">
                  <code>{uiCode || "// 此应用没有 UI 代码"}</code>
                </pre>
              )}
            </div>
          )}
        </div>

        <div className="modal-footer">
          {(tab === "backend" || tab === "ui") && (
            <>
              {editing ? (
                <>
                  <button
                    className="btn btn--primary"
                    onClick={handleSave}
                    disabled={saving}
                  >
                    {saving ? "保存中..." : "保存"}
                  </button>
                  <button
                    className="btn btn--secondary"
                    onClick={() => {
                      setEditing(false);
                      setBackendCode(app.backend_code);
                      setUiCode(app.ui_code ?? "");
                    }}
                  >
                    取消
                  </button>
                </>
              ) : (
                <button
                  className="btn btn--secondary"
                  onClick={() => setEditing(true)}
                >
                  编辑代码
                </button>
              )}
            </>
          )}
          <div className="modal-footer__spacer" />
          <button
            className="btn btn--secondary"
            onClick={loadApp}
            title="刷新"
          >
            <RefreshCw size={14} />
          </button>
          {app.is_packaged ? (
            <button
              className="btn btn--primary"
              onClick={handleRun}
            >
              <Play size={14} /> 启动应用
            </button>
          ) : (
            <button
              className="btn btn--primary"
              onClick={handlePackage}
              disabled={packaging}
            >
              <Package size={14} />
              {packaging ? "打包中..." : "打包为 EXE"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
