// 系统资源管理弹窗：磁盘清理 + 文件索引 + 进程。
//
// 设计取舍：
// - **弹窗而不是新窗口**：菜单点开就在当前窗口浮出，Esc / 点遮罩关闭。
// - **默认落在磁盘清理**：这是用户最常来的地方，而它进来就在「垃圾扫描」
//   模式（规则驱动、秒级、不做全盘扫描）。
// - 数据是**真值**：CPU/内存/磁盘占用全部来自 Rust 侧，进度条来自
//   `cleanup-progress` 事件（每完成一条规则推一次），不是假动效。
// - 动效克制：只做"数值变化"与"进度推进"的过渡，不做无意义的粒子动画，
//   免得刷新时界面一直在抖。

import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  Activity,
  Database,
  Info,
  Keyboard,
  Package,
  Trash2,
  X,
} from "lucide-react";
import { CleanupPane } from "./system/CleanupPane";
import { IndexPane } from "./system/IndexPane";
import { ProcessPane } from "./system/ProcessPane";

type Tab = "cleanup" | "index" | "processes";

export function SystemPanel({ onClose }: { onClose: () => void }) {
  // 磁盘清理是默认落点：垃圾扫描秒级出结果，用户点开就能看到东西。
  const [tab, setTab] = useState<Tab>("cleanup");

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="syspanel-overlay"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="syspanel" role="dialog" aria-modal="true" aria-label="系统资源管理">
        <header className="syspanel-head">
          <span className="syspanel-title">
            <Activity size={15} aria-hidden="true" />
            系统资源管理
          </span>
          <div className="syspanel-tabs" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={tab === "cleanup"}
              className={`syspanel-tab${tab === "cleanup" ? " is-active" : ""}`}
              onClick={() => setTab("cleanup")}
            >
              <Trash2 size={13} aria-hidden="true" />
              磁盘清理
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "index"}
              className={`syspanel-tab${tab === "index" ? " is-active" : ""}`}
              onClick={() => setTab("index")}
            >
              <Database size={13} aria-hidden="true" />
              文件索引
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === "processes"}
              className={`syspanel-tab${tab === "processes" ? " is-active" : ""}`}
              onClick={() => setTab("processes")}
            >
              <Activity size={13} aria-hidden="true" />
              进程
            </button>
          </div>
          <button type="button" className="syspanel-close" onClick={onClose} aria-label="关闭">
            <X size={16} />
          </button>
        </header>
        <div className="syspanel-body">
          {tab === "cleanup" && <CleanupPane />}
          {tab === "index" && <IndexPane />}
          {tab === "processes" && <ProcessPane />}
        </div>
      </div>
    </div>
  );
}

// ── 关于 ──────────────────────────────────────────────────────────────

const ABOUT_FEATURES = [
  "资源管理器：多标签、目录树、拖放、右键菜单",
  "多格式预览：文本 / 图片 / PDF / Office / 视频",
  "AI 右栏：选中内容直接提问",
  "系统资源管理：规则驱动的磁盘清理、本地文件索引、进程查看与结束",
];

const ABOUT_SHORTCUTS = [
  ["F5", "刷新当前目录"],
  ["Ctrl+Shift+Esc", "打开系统资源管理"],
  ["Esc", "关闭弹窗 / 菜单"],
];

export function AboutDialog({ onClose }: { onClose: () => void }) {
  const [version, setVersion] = useState("");

  useEffect(() => {
    void getVersion()
      .then(setVersion)
      .catch(() => setVersion(""));
  }, []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  return (
    <div
      className="syspanel-overlay"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="about-card" role="dialog" aria-modal="true" aria-label="关于">
        <header className="about-head">
          <span className="about-mark">
            <Activity size={18} aria-hidden="true" />
          </span>
          <div>
            <h3>Hermix 文件查看器</h3>
            <p className="dim">{version ? `版本 ${version}` : "独立文件浏览器"}</p>
          </div>
          <button type="button" className="syspanel-close" onClick={onClose} aria-label="关闭">
            <X size={16} />
          </button>
        </header>

        <section className="about-section">
          <h4>
            <Package size={13} aria-hidden="true" />
            能力
          </h4>
          <ul>
            {ABOUT_FEATURES.map((feature) => (
              <li key={feature}>{feature}</li>
            ))}
          </ul>
        </section>

        <section className="about-section">
          <h4>
            <Info size={13} aria-hidden="true" />
            技术
          </h4>
          <p className="dim">
            Rust + Tauri 2 内核，React 19 + Vite 前端；预览引擎与桌面端共用
            <code>@hermix/preview</code>。磁盘清理是规则驱动的（BleachBit / CCleaner
            的规则集 + 禁区校验 + 二次确认），文件索引读 NTFS MFT（类 Everything）。
          </p>
        </section>

        <section className="about-section">
          <h4>
            <Keyboard size={13} aria-hidden="true" />
            快捷键
          </h4>
          <dl className="about-keys">
            {ABOUT_SHORTCUTS.map(([key, text]) => (
              <div key={key}>
                <dt>
                  <kbd>{key}</kbd>
                </dt>
                <dd>{text}</dd>
              </div>
            ))}
          </dl>
        </section>

        <footer className="about-foot">
          <span className="dim">MIT License · 本地运行，不上传任何数据</span>
          <button type="button" className="pane-btn" onClick={onClose}>
            关闭
          </button>
        </footer>
      </div>
    </div>
  );
}
