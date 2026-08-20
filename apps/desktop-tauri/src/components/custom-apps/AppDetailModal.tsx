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
  BookOpen,
  Tag,
  Download,
  RefreshCcw,
  Terminal,
  CheckCircle2,
  Box,
  HardDrive,
} from "lucide-react";
import type { CustomAppResponse } from "../../lib/customAppApi";
import {
  getCustomApp,
  packageCustomApp,
  runCustomApp,
  updateCustomApp,
  openCustomAppDist,
} from "../../lib/customAppApi";
import { formatCategory, formatPermission, permHint } from "./labels";
import { resolveAppIconEl } from "./iconResolver";

interface AppDetailModalProps {
  appId: string;
  onClose: () => void;
  onChanged: () => void;
}

type Tab = "overview" | "code" | "readme";

export function AppDetailModal({ appId, onClose, onChanged }: AppDetailModalProps) {
  const [app, setApp] = useState<CustomAppResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<Tab>("overview");
  const [editing, setEditing] = useState(false);
  const [indexHtml, setIndexHtml] = useState("");
  const [appSource, setAppSource] = useState("");
  const [codeFile, setCodeFile] = useState<"app_source" | "index_html">("app_source");
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
      setIndexHtml(data.index_html ?? "");
      setAppSource(data.app_source ?? "");
      // Default to app_source if available, otherwise index_html
      setCodeFile(data.app_source ? "app_source" : "index_html");
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
      const updates: Parameters<typeof updateCustomApp>[1] = {};
      if (appSource !== (app.app_source ?? "")) {
        updates.app_source = appSource;
      }
      if (indexHtml !== (app.index_html ?? "")) {
        updates.index_html = indexHtml;
      }
      const updated = await updateCustomApp(appId, updates);
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
    setIndexHtml(app.index_html ?? "");
    setAppSource(app.app_source ?? "");
  };

  const handlePackage = async () => {
    setPackaging(true);
    setError(null);
    try {
      const result = await packageCustomApp(appId, "sidecar");
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
            {resolveAppIconEl(manifest, {
              spanClassName: "modal-header__icon",
              imgClassName: "modal-header__icon-img",
              size: 24,
            })}
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
          <button
            className={`modal-tab ${tab === "readme" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("readme")}
          >
            <BookOpen size={14} /> 说明
          </button>
        </div>

        <div className="modal-body">
          {error && <div className="modal-alert modal-alert--error">{error}</div>}
          {success && <div className="modal-alert modal-alert--success">{success}</div>}

          {tab === "overview" && (
            <div className="app-detail-overview">
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
                    <span className="app-detail-meta__value">{manifest.category ? formatCategory(manifest.category) : "—"}</span>
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
                      <span key={p} className="app-detail-perms__item" title={`${permHint(p)}（${p}）`}>
                        <Shield size={12} />
                        {formatPermission(p)}
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
                {app.app_source != null && (
                  <button
                    className={`app-detail-filebar__item ${codeFile === "app_source" ? "app-detail-filebar__item--active" : ""}`}
                    onClick={() => setCodeFile("app_source")}
                  >
                    <FileCode2 size={14} /> main.tsx
                  </button>
                )}
                <button
                  className={`app-detail-filebar__item ${codeFile === "index_html" ? "app-detail-filebar__item--active" : ""}`}
                  onClick={() => setCodeFile("index_html")}
                >
                  <FileCode2 size={14} /> index.html
                </button>
              </div>
              <div className="app-detail-code__editor-wrap">
                {editing ? (
                  <textarea
                    className="app-detail-code__editor"
                    value={codeFile === "app_source" ? appSource : indexHtml}
                    onChange={(e) => {
                      if (codeFile === "app_source") {
                        setAppSource(e.target.value);
                      } else {
                        setIndexHtml(e.target.value);
                      }
                    }}
                    spellCheck={false}
                    placeholder={codeFile === "app_source" ? "TypeScript/React 源代码" : "HTML 源代码"}
                  />
                ) : (
                  <pre className="app-detail-code__view">
                    <code>
                      {(codeFile === "app_source" ? appSource : indexHtml) ||
                        `// 此应用没有 ${codeFile === "app_source" ? "TypeScript" : "HTML"} 源代码`}
                    </code>
                  </pre>
                )}
              </div>
            </div>
          )}

          {tab === "readme" && (
            <div className="app-detail-readme">
              {/* Badges */}
              <div className="app-detail-readme__badges">
                <span className="app-detail-badge app-detail-badge--version">
                  <Tag size={11} /> v{manifest.version}
                </span>
                <span className="app-detail-badge app-detail-badge--author">
                  <User size={11} /> {manifest.author || "未知"}
                </span>
                <span className="app-detail-badge app-detail-badge--category">
                  <Folder size={11} /> {manifest.category ? formatCategory(manifest.category) : "其他"}
                </span>
                {is_packaged ? (
                  <span className="app-detail-badge app-detail-badge--success">
                    <CheckCircle2 size={11} /> 已打包
                  </span>
                ) : (
                  <span className="app-detail-badge app-detail-badge--muted">
                    <Box size={11} /> 未打包
                  </span>
                )}
                <span className="app-detail-badge app-detail-badge--info">
                  <Maximize size={11} /> {manifest.window_config.width}x{manifest.window_config.height}
                </span>
                <span className="app-detail-badge app-detail-badge--info">
                  <HardDrive size={11} /> hx-shell
                </span>
              </div>

              {/* Description */}
              <section className="app-detail-readme__section">
                <h4 className="app-detail-readme__heading">简介</h4>
                <p className="app-detail-readme__text">{manifest.description}</p>
                {manifest.long_description && (
                  <pre className="app-detail-readme__long">{manifest.long_description}</pre>
                )}
              </section>

              {/* Tags */}
              {manifest.tags.length > 0 && (
                <section className="app-detail-readme__section">
                  <h4 className="app-detail-readme__heading">标签</h4>
                  <div className="app-detail-readme__tags">
                    {manifest.tags.map((t) => (
                      <span key={t} className="app-detail-badge app-detail-badge--muted">{t}</span>
                    ))}
                  </div>
                </section>
              )}

              {/* Permissions */}
              <section className="app-detail-readme__section">
                <h4 className="app-detail-readme__heading">权限</h4>
                {manifest.permissions.length > 0 ? (
                  <div className="app-detail-readme__perms">
                    {manifest.permissions.map((p) => (
                      <span key={p} className="app-detail-badge app-detail-badge--perm" title={p}>
                        <Shield size={11} /> {formatPermission(p)}
                      </span>
                    ))}
                  </div>
                ) : (
                  <p className="app-detail-readme__text app-detail-readme__text--muted">此应用不需要特殊权限</p>
                )}
              </section>

              {/* Usage */}
              <section className="app-detail-readme__section">
                <h4 className="app-detail-readme__heading">
                  <Terminal size={14} /> 使用方法
                </h4>
                <div className="app-detail-readme__text">
                  <p>1. 点击右下角"运行"按钮在本地启动应用</p>
                  <p>2. 或点击"打包"生成独立 EXE，双击即可运行</p>
                  <p>3. 打包后的 EXE 无需安装，用户电脑需有 WebView2 运行时（Windows 10+ 自带）</p>
                </div>
              </section>

              {/* Update */}
              <section className="app-detail-readme__section">
                <h4 className="app-detail-readme__heading">
                  <RefreshCcw size={14} /> 更新方法
                </h4>
                <div className="app-detail-readme__text">
                  <p>1. 在"代码"标签页编辑源码后点击"保存"</p>
                  <p>2. 重新"打包"生成最新 EXE</p>
                  <p>3. 或在对话中让 LLM 修改应用，修改后重新打包</p>
                </div>
              </section>

              {/* Distribution */}
              <section className="app-detail-readme__section">
                <h4 className="app-detail-readme__heading">
                  <Download size={14} /> 分发方法
                </h4>
                <div className="app-detail-readme__text">
                  <p>1. 打包后在 dist/ 目录生成独立 EXE（单文件，含 HTML 注入）</p>
                  <p>2. 将 EXE 直接发给用户，双击即跑</p>
                  <p>3. 目标电脑需要 Windows 10+ 和 WebView2 运行时（系统自带）</p>
                  <p>4. 无需安装 Node.js、Rust 或任何开发工具链</p>
                </div>
              </section>

              {/* Tech */}
              <section className="app-detail-readme__section">
                <h4 className="app-detail-readme__heading">技术栈</h4>
                <div className="app-detail-readme__text">
                  <p>运行时: hx-shell (wry + WebView2, ~1.2MB)</p>
                  <p>API: window.hx.* (writeFile / readFile / readDir / exec / getInfo / fileUrl 等)</p>
                  <p>协议: hxfile:// 本地文件访问</p>
                  <p>打包: HTML 尾部注入 hx-shell.exe 生成单文件 EXE</p>
                </div>
              </section>
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
            <>
              <button
                className="app-detail-btn app-detail-btn--secondary"
                onClick={handlePackage}
                disabled={packaging}
                title="重新打包为可分发到其他电脑的独立 EXE"
              >
                <RefreshCw size={14} />
                {packaging ? "打包中..." : "重新打包"}
              </button>
              <button
                className="app-detail-btn app-detail-btn--secondary"
                onClick={handleOpenDist}
                title="打开打包后的应用目录"
              >
                <FolderOpen size={14} /> 打开目录
              </button>
            </>
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
