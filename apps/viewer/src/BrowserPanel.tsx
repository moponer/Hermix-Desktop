// 浏览器面板：管理 **agent 用的系统浏览器会话**（通道 B）。
//
// 谁在用这些会话：经 MCP 接入的任意外部 AI（Claude Code / Cursor …）调用
// `browser_open` / `browser_navigate` / `browser_screenshot` … 时操作的就是
// 这里列出的会话。这个面板让用户能看见「agent 现在在哪个浏览器里干什么」，
// 并在需要时手动收摊。
//
// 隔离规则（用户诉求「多个界面多个 agent 同时操作不会乱」）：
// **每个会话 id = 一个独立浏览器进程 + 独立 profile**（cookie / 登录态
// 互不干扰）。所以面板里新建会话时必须给一个 id——不同 agent 用不同 id。

import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Globe, Plus, RefreshCw, Trash2, X } from "lucide-react";
import "./browser-panel.css";

interface BrowserSession {
  id: string;
  headed: boolean;
  url: string;
  pid: number;
}

export function BrowserPanel({ onClose }: { onClose: () => void }) {
  const [sessions, setSessions] = useState<BrowserSession[]>([]);
  const [sessionId, setSessionId] = useState("default");
  const [url, setUrl] = useState("");
  const [headed, setHeaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const refresh = useCallback(async () => {
    try {
      setSessions(await invoke<BrowserSession[]>("browser_sessions"));
    } catch {
      setSessions([]);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => window.clearInterval(timer);
  }, [refresh]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [onClose]);

  const open = useCallback(async () => {
    const target = url.trim();
    if (!target) {
      setError("先填一个网址");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await invoke("browser_open", { session: sessionId.trim() || "default", url: target, headed });
      setUrl("");
      await refresh();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }, [headed, refresh, sessionId, url]);

  const closeSession = useCallback(
    async (id: string) => {
      try {
        await invoke("browser_close", { session: id });
      } catch (e) {
        setError(typeof e === "string" ? e : String(e));
      }
      await refresh();
    },
    [refresh],
  );

  return (
    <div
      className="bpanel-overlay"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="bpanel" role="dialog" aria-modal="true" aria-label="浏览器会话">
        <header className="bpanel-head">
          <span className="bpanel-title">
            <Globe size={15} aria-hidden="true" />
            浏览器会话
          </span>
          <button type="button" className="bpanel-close" onClick={onClose} aria-label="关闭">
            <X size={16} />
          </button>
        </header>

        <div className="bpanel-body">
          <section className="bpanel-block">
            <p className="bpanel-hint">
              agent 通过 MCP 工具（browser_open / browser_navigate / browser_screenshot …）操作这里的会话。
              <strong>每个会话 id = 一个独立浏览器进程 + 独立 profile</strong>
              ，cookie 与登录态互不干扰，不同 agent 用不同 id 即可并行。
            </p>
            <div className="bpanel-form">
              <label className="bpanel-field">
                <span className="bpanel-label">会话 id</span>
                <input
                  className="bpanel-input"
                  value={sessionId}
                  onChange={(e) => setSessionId(e.target.value)}
                  placeholder="default"
                  spellCheck={false}
                />
              </label>
              <label className="bpanel-field bpanel-field-grow">
                <span className="bpanel-label">网址</span>
                <input
                  className="bpanel-input"
                  value={url}
                  onChange={(e) => setUrl(e.target.value)}
                  placeholder="https://example.com"
                  spellCheck={false}
                />
              </label>
              <label className="bpanel-check" title="有头 = 弹出真实浏览器窗口，用户看得见也能接管">
                <input type="checkbox" checked={headed} onChange={(e) => setHeaded(e.target.checked)} />
                <span>有头</span>
              </label>
              <button
                type="button"
                className="tb-btn bpanel-go"
                onClick={() => void open()}
                disabled={busy}
              >
                <Plus size={14} className="icon-sm" />
                打开
              </button>
            </div>
            {error && <p className="bpanel-error">{error}</p>}
          </section>

          <section className="bpanel-block">
            <div className="bpanel-block-head">
              <span className="bpanel-label">当前会话（{sessions.length}）</span>
              <button
                type="button"
                className="tb-btn tb-icon"
                onClick={() => void refresh()}
                title="刷新列表"
                aria-label="刷新会话列表"
              >
                <RefreshCw size={13} className="icon-sm" />
              </button>
            </div>
            {sessions.length === 0 ? (
              <p className="bpanel-empty">还没有浏览器会话。agent 调用 browser_open 后会出现在这里。</p>
            ) : (
              <ul className="bpanel-list">
                {sessions.map((s) => (
                  <li key={s.id} className="bpanel-item">
                    <span className="bpanel-item-id" title={s.id}>
                      {s.id}
                    </span>
                    <span className={`bpanel-chip${s.headed ? " is-headed" : ""}`}>
                      {s.headed ? "有头" : "无头"}
                    </span>
                    <span className="bpanel-item-url" title={s.url}>
                      {s.url || "—"}
                    </span>
                    <button
                      type="button"
                      className="tb-btn tb-icon"
                      onClick={() => void closeSession(s.id)}
                      title="关闭会话"
                      aria-label={`关闭会话 ${s.id}`}
                    >
                      <Trash2 size={13} className="icon-sm" />
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>
        </div>
      </div>
    </div>
  );
}
