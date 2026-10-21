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
import { CodeView } from "../CodeView";
import { Button, IconButton, Modal, SkeletonCard } from "../ui";
import { PACKAGING, SAVE, SAVED, SAVING } from "../strings";

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
      setSuccess(SAVED);
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
      <Modal
        open
        onClose={onClose}
        width="wide"
        scrim
        title="应用详情"
        bodyClassName="p-5"
      >
        <SkeletonCard lines={5} />
      </Modal>
    );
  }

  if (!app) {
    return (
      <Modal
        open
        onClose={onClose}
        width="wide"
        scrim
        title="应用详情"
        bodyClassName="p-5"
      >
        <div className="modal-error">应用不存在</div>
      </Modal>
    );
  }

  const { manifest, is_packaged, dist_path } = app;
  // 只读态走站内 CodeView：虚拟行号（>300 行才虚拟化）+ refractor 高亮 +
  // 主题跟随，与聊天/预览两处共用同一套渲染，不另起一套 <pre>。
  const codeSource = codeFile === "app_source" ? appSource : indexHtml;
  const codeLanguage = codeFile === "app_source" ? "tsx" : "html";
  /** 编辑态只在「代码」页存在 —— 底栏的主按钮归属按它判断。 */
  const codeEditing = tab === "code" && editing;

  return (
    /* 弹窗壳收编到 ui/Modal：焦点陷阱 / Esc / 焦点归还 / role=dialog +
       aria-modal 一次性补齐（此前这个弹窗连 role="dialog" 都没有）。 */
    <Modal
      open
      onClose={onClose}
      width="wide"
      scrim
      title={
        <span className="modal-header__title">
          {resolveAppIconEl(manifest, {
            spanClassName: "modal-header__icon",
            imgClassName: "modal-header__icon-img",
            size: 24,
          })}
          <span className="truncate">{manifest.name}</span>
          <span className="modal-header__version">v{manifest.version}</span>
          {is_packaged && <span className="modal-header__badge modal-header__badge--success">已打包</span>}
        </span>
      }
      bodyClassName="flex flex-col overflow-y-auto p-0"
      footer={
        /* 一个底栏只允许一个主按钮（问题 47）：此前「保存」与「运行」都是
           实心主按钮，且左右两套方向。主按钮恒在最右，其余降级
           secondary / ghost。 */
        <div className="flex w-full items-center justify-end gap-2">
          {codeEditing && (
            <Button variant="ghost" size="base" onClick={cancelEdit}>
              取消
            </Button>
          )}
          {tab === "code" && !editing && (
            <Button
              variant="secondary"
              size="base"
              icon={<Pencil className="icon-md" />}
              onClick={() => setEditing(true)}
            >
              编辑代码
            </Button>
          )}
          <IconButton
            icon={<RefreshCw className={`icon-md ${loading ? "spin" : ""}`} />}
            label="刷新"
            shape="circle"
            size="base"
            variant="ghost"
            onClick={loadApp}
            disabled={loading}
          />
          {is_packaged ? (
            <>
              <Button
                variant="secondary"
                size="base"
                icon={<RefreshCw className="icon-md" />}
                onClick={handlePackage}
                disabled={packaging}
                title="重新打包为可分发到其他电脑的独立 EXE"
              >
                {packaging ? PACKAGING : "重新打包"}
              </Button>
              <Button
                variant="secondary"
                size="base"
                icon={<FolderOpen className="icon-md" />}
                onClick={handleOpenDist}
                title="打开打包后的应用目录"
              >
                打开目录
              </Button>
            </>
          ) : (
            <Button
              variant="secondary"
              size="base"
              icon={<Package className="icon-md" />}
              onClick={handlePackage}
              disabled={packaging}
            >
              {packaging ? PACKAGING : "打包"}
            </Button>
          )}
          {/* 主按钮恒在最后一位：编辑态是「保存」，只读态是「运行」，
              同一时刻只存在一个实心主按钮。 */}
          <Button
            variant={codeEditing ? "secondary" : "primary"}
            size="base"
            icon={<Play className="icon-md" />}
            onClick={handleRun}
          >
            运行
          </Button>
          {codeEditing && (
            <Button
              variant="primary"
              size="base"
              icon={<Save className="icon-md" />}
              onClick={handleSave}
              disabled={saving}
            >
              {saving ? SAVING : SAVE}
            </Button>
          )}
        </div>
      }
    >
        <div className="modal-tabs shrink-0">
          <button
            className={`modal-tab ${tab === "overview" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("overview")}
          >
            <LayoutGrid className="icon-md" /> 概览
          </button>
          <button
            className={`modal-tab ${tab === "code" ? "modal-tab--active" : ""}`}
            onClick={() => {
              setTab("code");
              setEditing(false);
            }}
          >
            <Code2 className="icon-md" /> 代码
          </button>
          <button
            className={`modal-tab ${tab === "readme" ? "modal-tab--active" : ""}`}
            onClick={() => setTab("readme")}
          >
            <BookOpen className="icon-md" /> 说明
          </button>
        </div>

          {error && (
            <div className="modal-alert modal-alert--error" role="alert">
              <span className="modal-alert__text">{error}</span>
              <button
                type="button"
                className="modal-alert__dismiss"
                onClick={() => setError(null)}
                title="关闭"
                aria-label="关闭错误提示"
              >
                <X className="icon-sm" />
              </button>
            </div>
          )}
          {success && (
            <div className="modal-alert modal-alert--success">
              <span className="modal-alert__text">{success}</span>
            </div>
          )}

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
                    <User className="icon-md" />
                    <span className="app-detail-meta__label">作者</span>
                    <span className="app-detail-meta__value">{manifest.author || "—"}</span>
                  </div>
                  <div className="app-detail-meta__item">
                    <Folder className="icon-md" />
                    <span className="app-detail-meta__label">分类</span>
                    <span className="app-detail-meta__value">{manifest.category ? formatCategory(manifest.category) : "—"}</span>
                  </div>
                  <div className="app-detail-meta__item">
                    <Maximize className="icon-md" />
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
                        <Shield className="icon-sm" />
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
                    <FileCode2 className="icon-md" /> main.tsx
                  </button>
                )}
                <button
                  className={`app-detail-filebar__item ${codeFile === "index_html" ? "app-detail-filebar__item--active" : ""}`}
                  onClick={() => setCodeFile("index_html")}
                >
                  <FileCode2 className="icon-md" /> index.html
                </button>
              </div>
              {/* preview-pane class 让 code-view.css 里的 .code-viewport /
                  .code-gutter / .code-pre 规则命中（这套样式统一挂在
                  .preview-pane 作用域下，该 class 自身无独立样式，零副作用） */}
              <div className="app-detail-code__editor-wrap preview-pane">
                {editing ? (
                  <textarea
                    className="app-detail-code__editor"
                    value={codeSource}
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
                ) : codeSource ? (
                  <CodeView source={codeSource} language={codeLanguage} />
                ) : (
                  <div className="app-detail-code__empty">
                    此应用没有 {codeFile === "app_source" ? "TypeScript" : "HTML"} 源代码
                  </div>
                )}
              </div>
            </div>
          )}

          {tab === "readme" && (
            <div className="app-detail-readme">
              {/* Badges */}
              <div className="app-detail-readme__badges">
                <span className="app-detail-badge app-detail-badge--version">
                  <Tag className="icon-sm" /> v{manifest.version}
                </span>
                <span className="app-detail-badge app-detail-badge--author">
                  <User className="icon-sm" /> {manifest.author || "未知"}
                </span>
                <span className="app-detail-badge app-detail-badge--category">
                  <Folder className="icon-sm" /> {manifest.category ? formatCategory(manifest.category) : "其他"}
                </span>
                {is_packaged ? (
                  <span className="app-detail-badge app-detail-badge--success">
                    <CheckCircle2 className="icon-sm" /> 已打包
                  </span>
                ) : (
                  <span className="app-detail-badge app-detail-badge--muted">
                    <Box className="icon-sm" /> 未打包
                  </span>
                )}
                <span className="app-detail-badge app-detail-badge--info">
                  <Maximize className="icon-sm" /> {manifest.window_config.width}x{manifest.window_config.height}
                </span>
                <span className="app-detail-badge app-detail-badge--info">
                  <HardDrive className="icon-sm" /> hx-shell
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
                        <Shield className="icon-sm" /> {formatPermission(p)}
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
                  <Terminal className="icon-md" /> 使用方法
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
                  <RefreshCcw className="icon-md" /> 更新方法
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
                  <Download className="icon-md" /> 分发方法
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
    </Modal>
  );
}
