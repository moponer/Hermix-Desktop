// AppDetailModal — redesigned app details with card-based overview and code editor

import { useState, useEffect } from "react";
import {
  X,
  Package,
  FolderOpen,
  Play,
  RefreshCw,
  Code2,
  LayoutGrid,
  FileCode2,
  User,
  Folder,
  Maximize,
  Shield,
  Save,
  Pencil,
} from "lucide-react";
import type { CustomAppResponse } from "../../lib/customAppApi";
import {
  getCustomApp,
  packageCustomApp,
  runCustomApp,
  updateCustomApp,
  openCustomAppDist,
} from "../../lib/customAppApi";

interface AppDetailModalProps {
  appId: string;
  onClose: () => void;
  onChanged: () => void;
}

type Tab = "overview" | "code";
type CodeFile = "backend" | "ui";

const PERM_HINTS: Record<string, string> = {
  "fs-read": "读取本地文件系统",
  "fs-write": "写入本地文件系统",
  process: "启动外部进程或命令",
  network: "发起网络请求",
};

function permHint(name: string): string {
  return PERM_HINTS[name] ?? `应用请求的权限: ${name}`;
}

export function AppDetailModal({ appId, onClose, onChanged }: AppDetailModalProps) {
  const [app, setApp] = useState<CustomAppResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<Tab>("overview");
  const [codeFile, setCodeFile] = useState<CodeFile>("backend");
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

  const cancelEdit = () => {
    if (!app) return;
    setEditing(false);
    setBackendCode(app.backend_code);
    setUiCode(app.ui_code ?? "");
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

  const handleOpenDist = async () => {
    if (!app?.dist_path) return;
    setError(null);
    try {
      await openCustomAppDist(app.dist_path);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  const setActiveCode = (value: string) => {
    if (codeFile === "backend") {
      setBackendCode(value);
    } else {
      setUiCode(value);
    }
  };

  const activeCode = codeFile === "backend" ? backendCode : uiCode;

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

  const { manifest, is_packaged, dist_path } = app;

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal-content modal-content--wide app-detail-modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <div className="modal-header__title">
            {manifest.icon_file ? (
              <img
                className="modal-header__icon-img"
                src={`/api/custom-apps/${encodeURIComponent(appId)}/icon`}
                alt={manifest.name}
                draggable={false}
              />
            ) : (
              <span className="modal-header__icon">{manifest.icon}</span>
            )}
            <h2>{manifest.name}</h2>
            <span className="modal-header__version">v{manifest.version}</span>
            {is_packaged && <span className="modal-header__badge modal-header__badge--success">已打包</span>}
          </div>
          <button className="modal-close" onClick={onClose} aria-label="关闭">
            <X size={18} />
          </button>
        </div>

        <div className="modal-tabs">
          <button
            className={`modal-tab ${tab === "overview" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("overview")}
          >
            <LayoutGrid size={14} /> 概览
          </button>
          <button
            className={`modal-tab ${tab === "code" ? "modal-tab--active" : ""}`}
            onClick={() => {
              setTab("code");
              setEditing(false);
            }}
          >
            <Code2 size={14} /> 代码
          </button>
        </div>

        <div className="modal-body">
          {error && <div className="modal-alert modal-alert--error">{error}</div>}
          {success && <div className="modal-alert modal-alert--success">{success}</div>}

          {tab === "overview" && (
            <div className="app-detail-overview">
              <section className="app-detail-hero">
                <div className="app-detail-hero__main">
                  <div className="app-detail-hero__icon-wrap">
                    {manifest.icon_file ? (
                      <img
                        className="app-detail-hero__icon-img"
                        src={`/api/custom-apps/${encodeURIComponent(appId)}/icon`}
                        alt={manifest.name}
                        draggable={false}
                      />
                    ) : (
                      <span className="app-detail-hero__icon">{manifest.icon}</span>
                    )}
                  </div>
                  <div className="app-detail-hero__text">
                    <h3 className="app-detail-hero__name">{manifest.name}</h3>
                    <p className="app-detail-hero__desc">{manifest.description}</p>
                    {manifest.long_description && (
                      <p className="app-detail-hero__long">{manifest.long_description}</p>
                    )}
                  </div>
                </div>
                <div className="app-detail-hero__actions">
                  <button
                    className="app-detail-btn app-detail-btn--primary"
                    onClick={handleRun}
                    title="编译并启动应用（本地测试）"
                  >
                    <Play size={14} /> 运行
                  </button>
                  {is_packaged ? (
                    <button
                      className="app-detail-btn app-detail-btn--secondary"
                      onClick={handleOpenDist}
                      title="打开打包后的应用目录"
                    >
                      <FolderOpen size={14} /> 打开目录
                    </button>
                  ) : (
                    <button
                      className="app-detail-btn app-detail-btn--secondary"
                      onClick={handlePackage}
                      disabled={packaging}
                      title="打包为可分发独立 EXE"
                    >
                      <Package size={14} />
                      {packaging ? "打包中..." : "打包"}
                    </button>
                  )}
                </div>
              </section>

              <section className="app-detail-section">
                <h4 className="app-detail-section__title">应用信息</h4>
                <div className="app-detail-meta">
                  <div className="app-detail-meta__item">
                    <span className="app-detail-meta__label">ID</span>
                    <code className="app-detail-meta__value">{manifest.id}</code>
                  </div>
                  <div className="app-detail-meta__item">
                    <User size={13} />
                    <span className="app-detail-meta__label">作者</span>
                    <span className="app-detail-meta__value">{manifest.author || "—"}</span>
                  </div>
                  <div className="app-detail-meta__item">
                    <Folder size={13} />
                    <span className="app-detail-meta__label">分类</span>
                    <span className="app-detail-meta__value">{manifest.category || "—"}</span>
                  </div>
                  <div className="app-detail-meta__item">
                    <Maximize size={13} />
                    <span className="app-detail-meta__label">窗口</span>
                    <span className="app-detail-meta__value">
                      {manifest.window_config.width} × {manifest.window_config.height}
                      {manifest.window_config.resizable ? " · 可调整" : " · 固定"}
                    </span>
                  </div>
                  {dist_path && (
                    <div className="app-detail-meta__item app-detail-meta__item--full">
                      <span className="app-detail-meta__label">输出目录</span>
                      <code className="app-detail-meta__value">{dist_path}</code>
                    </div>
                  )}
                </div>
              </section>

              <section className="app-detail-section">
                <h4 className="app-detail-section__title">权限声明</h4>
                {manifest.permissions.length > 0 ? (
                  <div className="app-detail-perms">
                    {manifest.permissions.map((p) => (
                      <span key={p} className="app-detail-perms__item" title={permHint(p)}>
                        <Shield size={12} />
                        {p}
                      </span>
                    ))}
                  </div>
                ) : (
                  <p className="app-detail-empty">此应用不需要特殊权限</p>
                )}
              </section>
            </div>
          )}

          {tab === "code" && (
            <div className="app-detail-code-panel">
              <div className="app-detail-filebar">
                <button
                  className={`app-detail-filebar__item ${codeFile === "backend" ? "app-detail-filebar__item--active" : ""}`}
                  onClick={() => setCodeFile("backend")}
                >
                  <FileCode2 size={14} /> backend.ts
                </button>
                <button
                  className={`app-detail-filebar__item ${codeFile === "ui" ? "app-detail-filebar__item--active" : ""}`}
                  onClick={() => setCodeFile("ui")}
                >
                  <FileCode2 size={14} /> ui.tsx
                </button>
              </div>
              <div className="app-detail-code__editor-wrap">
                {editing ? (
                  <textarea
                    className="app-detail-code__editor"
                    value={activeCode}
                    onChange={(e) => setActiveCode(e.target.value)}
                    spellCheck={false}
                    placeholder={codeFile === "ui" ? "此应用没有 UI 代码" : undefined}
                  />
                ) : (
                  <pre className="app-detail-code__view">
                    <code>{codeFile === "ui" && !uiCode ? "// 此应用没有 UI 代码" : activeCode}</code>
                  </pre>
                )}
              </div>
            </div>
          )}
        </div>

        <div className="modal-footer">
          {tab === "code" && (
            <div className="modal-footer__left">
              {editing ? (
                <>
                  <button
                    className="app-detail-btn app-detail-btn--primary"
                    onClick={handleSave}
                    disabled={saving}
                  >
                    <Save size={14} />
                    {saving ? "保存中..." : "保存"}
                  </button>
                  <button className="app-detail-btn app-detail-btn--ghost" onClick={cancelEdit}>
                    取消
                  </button>
                </>
              ) : (
                <button className="app-detail-btn app-detail-btn--secondary" onClick={() => setEditing(true)}>
                  <Pencil size={14} /> 编辑代码
                </button>
              )}
            </div>
          )}
          <div className="modal-footer__spacer" />
          <button
            className="app-detail-iconbtn"
            onClick={loadApp}
            disabled={loading}
            title="刷新"
            aria-label="刷新"
          >
            <RefreshCw size={14} className={loading ? "spin" : ""} />
          </button>
          <button className="app-detail-btn app-detail-btn--primary" onClick={handleRun}>
            <Play size={14} /> 运行
          </button>
          {is_packaged ? (
            <button
              className="app-detail-btn app-detail-btn--secondary"
              onClick={handleOpenDist}
              title="打开打包后的应用目录"
            >
              <FolderOpen size={14} /> 打开目录
            </button>
          ) : (
            <button
              className="app-detail-btn app-detail-btn--secondary"
              onClick={handlePackage}
              disabled={packaging}
            >
              <Package size={14} />
              {packaging ? "打包中..." : "打包"}
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
