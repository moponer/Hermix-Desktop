// 设置对话框：通用 / 搜索 / AI 助手 / MCP 服务。
//
// 为什么单独一个对话框而不是散在工具条里：查看器的可配项已经有四组（搜索
// 行为、AI 后端与模型、MCP 服务、通用偏好），散在各处等于没有配置面。
//
// 密钥处理：独立模型的 API Key **不**进设置文件（那里会被备份/同步），写进
// 同目录的 .env；输入框只显示「已配置 / 未配置」。

import { useCallback, useEffect, useState } from "react";
import {
  Check,
  Copy,
  Eye,
  EyeOff,
  FolderTree,
  Search,
  Settings2,
  Sparkles,
  Wrench,
  X,
} from "lucide-react";
import {
  DEFAULT_SETTINGS,
  loadAiStatus,
  loadMcpStatus,
  loadSettings,
  mcpClientConfig,
  saveSettings,
  setAiKey,
  startMcp,
  stopMcp,
  type AiStatus,
  type McpStatus,
  type ViewerSettings,
} from "./settings";

type TabId = "general" | "search" | "ai" | "mcp";

const TABS: { id: TabId; label: string; icon: typeof Settings2 }[] = [
  { id: "general", label: "通用", icon: Settings2 },
  { id: "search", label: "搜索", icon: Search },
  { id: "ai", label: "AI 助手", icon: Sparkles },
  { id: "mcp", label: "MCP 服务", icon: FolderTree },
];

export function SettingsDialog({
  open,
  onClose,
  onSaved,
}: {
  open: boolean;
  onClose: () => void;
  /** 保存成功后的回执（外层弹轻提示）。 */
  onSaved?: (message: string) => void;
}) {
  const [tab, setTab] = useState<TabId>("general");
  const [draft, setDraft] = useState<ViewerSettings>(DEFAULT_SETTINGS);
  const [status, setStatus] = useState<AiStatus | null>(null);
  const [mcp, setMcp] = useState<McpStatus | null>(null);
  const [keyDraft, setKeyDraft] = useState("");
  const [keyVisible, setKeyVisible] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setBusy(true);
    void Promise.all([loadSettings(), loadAiStatus(), loadMcpStatus()])
      .then(([s, st, mc]) => {
        setDraft(s);
        setStatus(st);
        setMcp(mc);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setBusy(false));
  }, [open]);

  // 切到 MCP 页时刷新一次状态（服务可能在设置之外被起停）。
  useEffect(() => {
    if (!open || tab !== "mcp") return;
    void loadMcpStatus().then(setMcp);
  }, [open, tab]);

  const patch = useCallback((fn: (s: ViewerSettings) => ViewerSettings) => {
    setDraft((s) => fn(s));
  }, []);

  const onSave = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const saved = await saveSettings(draft);
      setDraft(saved);
      setStatus(await loadAiStatus());
      onSaved?.("设置已保存");
      onClose();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }, [draft, onClose, onSaved]);

  const onSaveKey = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      await setAiKey(keyDraft);
      setKeyDraft("");
      const s = await loadSettings();
      setDraft(s);
      setStatus(await loadAiStatus());
      onSaved?.(keyDraft.trim() ? "密钥已写入 .env" : "密钥已清除");
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }, [keyDraft, onSaved]);

  const onCopyMcp = useCallback(async () => {
    const text = mcpClientConfig(draft.mcp.port);
    try {
      await navigator.clipboard.writeText(text);
      onSaved?.("MCP 客户端配置已复制");
    } catch {
      onSaved?.("复制失败，请手动选中复制");
    }
  }, [draft.mcp.port, onSaved]);

  const onMcpToggle = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      const st = mcp?.running ? await stopMcp() : await startMcp();
      setMcp(st);
      onSaved?.(st.running ? "MCP 服务已启动" : "MCP 服务已停止");
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
      void loadMcpStatus().then(setMcp);
    } finally {
      setBusy(false);
    }
  }, [mcp, onSaved]);

  if (!open) return null;

  return (
    <div
      className="set-overlay"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div className="set-card" role="dialog" aria-modal="true" aria-label="设置">
        <div className="set-head">
          <span className="set-title">设置</span>
          <button className="set-close" onClick={onClose} aria-label="关闭设置">
            <X size={15} />
          </button>
        </div>

        <div className="set-body">
          <nav className="set-nav" role="tablist" aria-label="设置分类">
            {TABS.map((t) => {
              const Icon = t.icon;
              return (
                <button
                  key={t.id}
                  role="tab"
                  aria-selected={tab === t.id}
                  className={`set-nav-item ${tab === t.id ? "is-active" : ""}`}
                  onClick={() => setTab(t.id)}
                >
                  <Icon size={14} />
                  <span>{t.label}</span>
                </button>
              );
            })}
          </nav>

          <div className="set-pane">
            {tab === "general" && (
              <>
                <label className="set-field">
                  <span className="set-label">启动时打开的目录</span>
                  <input
                    className="set-input"
                    value={draft.general.defaultDir}
                    placeholder="留空 = 上次目录 → 主目录 → 首个盘符"
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        general: { ...s.general, defaultDir: e.target.value },
                      }))
                    }
                  />
                </label>
                <label className="set-check">
                  <input
                    type="checkbox"
                    checked={draft.general.showHiddenByDefault}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        general: { ...s.general, showHiddenByDefault: e.target.checked },
                      }))
                    }
                  />
                  <span>启动时显示隐藏文件</span>
                </label>
                <label className="set-check">
                  <input
                    type="checkbox"
                    checked={draft.general.confirmCloseTab}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        general: { ...s.general, confirmCloseTab: e.target.checked },
                      }))
                    }
                  />
                  <span>关闭标签页前先确认</span>
                </label>
                <div className="set-row">
                  <span className="set-label">网页内核</span>
                  <div className="set-seg">
                    <button
                      className={`set-seg-btn ${draft.browser.kernel === "embedded" ? "is-active" : ""}`}
                      onClick={() =>
                        patch((s) => ({ ...s, browser: { ...s.browser, kernel: "embedded" } }))
                      }
                    >
                      内嵌视图
                    </button>
                    <button
                      className={`set-seg-btn ${draft.browser.kernel === "system" ? "is-active" : ""}`}
                      onClick={() =>
                        patch((s) => ({ ...s, browser: { ...s.browser, kernel: "system" } }))
                      }
                    >
                      系统浏览器
                    </button>
                  </div>
                </div>
                <label className="set-check">
                  <input
                    type="checkbox"
                    checked={draft.browser.headed}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        browser: { ...s.browser, headed: e.target.checked },
                      }))
                    }
                  />
                  <span>agent 浏览器默认有头（弹出真实窗口）</span>
                </label>
              </>
            )}

            {tab === "search" && (
              <>
                <div className="set-row">
                  <span className="set-label">触发方式</span>
                  <div className="set-seg">
                    <button
                      className={`set-seg-btn ${draft.search.mode === "live" ? "is-active" : ""}`}
                      onClick={() =>
                        patch((s) => ({ ...s, search: { ...s.search, mode: "live" } }))
                      }
                    >
                      输入即搜
                    </button>
                    <button
                      className={`set-seg-btn ${draft.search.mode === "enter" ? "is-active" : ""}`}
                      onClick={() =>
                        patch((s) => ({ ...s, search: { ...s.search, mode: "enter" } }))
                      }
                    >
                      回车才搜
                    </button>
                  </div>
                </div>
                <label className="set-field">
                  <span className="set-label">去抖（毫秒）</span>
                  <input
                    className="set-input"
                    type="number"
                    min={0}
                    max={2000}
                    step={50}
                    value={draft.search.debounceMs}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        search: { ...s.search, debounceMs: Number(e.target.value) || 0 },
                      }))
                    }
                  />
                </label>
                <div className="set-row">
                  <span className="set-label">搜索范围</span>
                  <select
                    className="set-select"
                    value={draft.search.scope}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        search: { ...s.search, scope: e.target.value as ViewerSettings["search"]["scope"] },
                      }))
                    }
                  >
                    <option value="auto">全盘优先（不可用时退回当前目录）</option>
                    <option value="global">只搜全盘索引</option>
                    <option value="local">只搜当前目录</option>
                  </select>
                </div>
                <label className="set-field">
                  <span className="set-label">最多返回条数</span>
                  <input
                    className="set-input"
                    type="number"
                    min={20}
                    max={2000}
                    step={50}
                    value={draft.search.limit}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        search: { ...s.search, limit: Number(e.target.value) || 300 },
                      }))
                    }
                  />
                </label>
                <label className="set-check">
                  <input
                    type="checkbox"
                    checked={draft.search.includeHidden}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        search: { ...s.search, includeHidden: e.target.checked },
                      }))
                    }
                  />
                  <span>搜索时包含隐藏文件</span>
                </label>
              </>
            )}

            {tab === "ai" && (
              <>
                <div className="set-row">
                  <span className="set-label">对话模型</span>
                  <div className="set-seg">
                    <button
                      className={`set-seg-btn ${draft.ai.mode === "inherit" ? "is-active" : ""}`}
                      onClick={() =>
                        patch((s) => ({ ...s, ai: { ...s.ai, mode: "inherit" } }))
                      }
                    >
                      继承主程序
                    </button>
                    <button
                      className={`set-seg-btn ${draft.ai.mode === "custom" ? "is-active" : ""}`}
                      onClick={() =>
                        patch((s) => ({ ...s, ai: { ...s.ai, mode: "custom" } }))
                      }
                    >
                      独立模型
                    </button>
                  </div>
                </div>

                <div className="set-note">
                  {status ? (
                    status.ready ? (
                      <>
                        当前生效：{status.source}
                        {status.provider ? ` · ${status.provider}` : ""} · {status.model}
                        {status.baseUrl ? ` · ${status.baseUrl}` : ""}
                      </>
                    ) : (
                      <>当前不可用：{status.problem ?? "配置不完整"}</>
                    )
                  ) : (
                    "正在读取后端状态…"
                  )}
                </div>

                {draft.ai.mode === "custom" && (
                  <>
                    <label className="set-field">
                      <span className="set-label">端点（OpenAI 兼容，含 /v1）</span>
                      <input
                        className="set-input"
                        value={draft.ai.baseUrl}
                        placeholder="https://api.example.com/v1"
                        onChange={(e) =>
                          patch((s) => ({ ...s, ai: { ...s.ai, baseUrl: e.target.value } }))
                        }
                      />
                    </label>
                    <label className="set-field">
                      <span className="set-label">模型 id</span>
                      <input
                        className="set-input"
                        value={draft.ai.model}
                        placeholder="例如 gpt-4o-mini"
                        onChange={(e) =>
                          patch((s) => ({ ...s, ai: { ...s.ai, model: e.target.value } }))
                        }
                      />
                    </label>
                    <div className="set-field">
                      <span className="set-label">
                        API Key（{draft.ai.hasKey ? "已配置，写在 .env" : "未配置"}）
                      </span>
                      <div className="set-key-row">
                        <input
                          className="set-input"
                          type={keyVisible ? "text" : "password"}
                          value={keyDraft}
                          placeholder={draft.ai.hasKey ? "留空则保持不变" : "粘贴密钥"}
                          onChange={(e) => setKeyDraft(e.target.value)}
                        />
                        <button
                          className="set-icon-btn"
                          onClick={() => setKeyVisible((v) => !v)}
                          aria-label={keyVisible ? "隐藏密钥" : "显示密钥"}
                          title={keyVisible ? "隐藏" : "显示"}
                        >
                          {keyVisible ? <EyeOff size={14} /> : <Eye size={14} />}
                        </button>
                        <button className="set-btn" onClick={() => void onSaveKey()}>
                          <Check size={13} />
                          <span>写入</span>
                        </button>
                      </div>
                    </div>
                  </>
                )}

                <label className="set-field">
                  <span className="set-label">系统提示（追加给模型）</span>
                  <textarea
                    className="set-textarea"
                    rows={4}
                    value={draft.ai.systemPrompt}
                    onChange={(e) =>
                      patch((s) => ({ ...s, ai: { ...s.ai, systemPrompt: e.target.value } }))
                    }
                  />
                </label>
                <div className="set-two">
                  <label className="set-field">
                    <span className="set-label">temperature</span>
                    <input
                      className="set-input"
                      type="number"
                      min={0}
                      max={2}
                      step={0.1}
                      value={draft.ai.temperature ?? ""}
                      onChange={(e) =>
                        patch((s) => ({
                          ...s,
                          ai: {
                            ...s.ai,
                            temperature:
                              e.target.value === "" ? null : Number(e.target.value),
                          },
                        }))
                      }
                    />
                  </label>
                  <label className="set-field">
                    <span className="set-label">max_tokens</span>
                    <input
                      className="set-input"
                      type="number"
                      min={256}
                      max={64000}
                      step={256}
                      value={draft.ai.maxTokens ?? ""}
                      onChange={(e) =>
                        patch((s) => ({
                          ...s,
                          ai: {
                            ...s.ai,
                            maxTokens:
                              e.target.value === "" ? null : Number(e.target.value),
                          },
                        }))
                      }
                    />
                  </label>
                </div>
              </>
            )}

            {tab === "mcp" && (
              <>
                <div className="set-note">
                  查看器自带的 MCP 服务能把「列目录 / 读文件 / 搜索 / 写文本 /
                  操作桌面 GUI」这些能力开放给任意客户端（Claude Desktop、Cursor、
                  任何走 MCP 的 agent）。服务只监听本机回环地址。
                </div>
                <label className="set-check">
                  <input
                    type="checkbox"
                    checked={draft.mcp.enabled}
                    onChange={(e) =>
                      patch((s) => ({ ...s, mcp: { ...s.mcp, enabled: e.target.checked } }))
                    }
                  />
                  <span>启动查看器时拉起本地 MCP 服务</span>
                </label>
                <label className="set-field">
                  <span className="set-label">端口</span>
                  <input
                    className="set-input"
                    type="number"
                    min={1024}
                    max={65535}
                    value={draft.mcp.port}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        mcp: { ...s.mcp, port: Number(e.target.value) || 3911 },
                      }))
                    }
                  />
                </label>
                <label className="set-check">
                  <input
                    type="checkbox"
                    checked={draft.mcp.allowRemote}
                    onChange={(e) =>
                      patch((s) => ({
                        ...s,
                        mcp: { ...s.mcp, allowRemote: e.target.checked },
                      }))
                    }
                  />
                  <span>允许非本机连接（会把文件操作能力暴露到网上，不建议）</span>
                </label>
                <div className="set-field">
                  <span className="set-label">服务状态</span>
                  <div className="set-key-row">
                    <span className={`set-state ${mcp?.running ? "is-on" : ""}`}>
                      {mcp?.running
                        ? `运行中 · ${mcp.endpoint ?? ""}`
                        : "未启动（只监听本机 127.0.0.1）"}
                    </span>
                    <button className="set-btn" onClick={() => void onMcpToggle()}>
                      {mcp?.running ? (
                        <>
                          <X size={13} />
                          <span>停止</span>
                        </>
                      ) : (
                        <>
                          <Check size={13} />
                          <span>启动</span>
                        </>
                      )}
                    </button>
                  </div>
                </div>
                <div className="set-field">
                  <span className="set-label">客户端配置</span>
                  <div className="set-key-row">
                    <code className="set-code">{mcpClientConfig(draft.mcp.port)}</code>
                    <button className="set-btn" onClick={() => void onCopyMcp()}>
                      <Copy size={13} />
                      <span>复制</span>
                    </button>
                  </div>
                </div>
                <div className="set-note">
                  提示：查看器也可以用命令行 <code>hermix-viewer --mcp</code> 以
                  stdio 方式给客户端拉起（适合配置里写 command 的场景）。
                </div>
              </>
            )}
          </div>
        </div>

        {error && <div className="set-error">{error}</div>}

        <div className="set-foot">
          <button className="set-btn" onClick={onClose}>
            <X size={13} />
            <span>取消</span>
          </button>
          <button className="set-btn is-primary" onClick={() => void onSave()} disabled={busy}>
            <Wrench size={13} />
            <span>保存</span>
          </button>
        </div>
      </div>
    </div>
  );
}
