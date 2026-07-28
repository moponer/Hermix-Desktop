import { useState, useEffect, useCallback } from "react";
import { X, Shield, AlertTriangle, Star, Settings, MessageCircle, Clock, Users, CheckCircle2 } from "lucide-react";
import { clsx } from "clsx";
import * as api from "../api";
import { useSafeTimeout } from "../hooks/useSafeTimeout";
import { toastError } from "../hooks/useToast";

interface RoomSettingsDrawerProps {
  roomId: string;
  roomName: string;
  onClose: () => void;
}

export function RoomSettingsDrawer({ roomId, roomName, onClose }: RoomSettingsDrawerProps) {
  const safeSetTimeout = useSafeTimeout();
  const [tab, setTab] = useState<"rules" | "general">("rules");
  const [globalConstraints, setGlobalConstraints] = useState<string[]>([]);
  const [newConstraint, setNewConstraint] = useState("");
  const [toolPolicyMode, setToolPolicyMode] = useState("blacklist");
  const [blacklistedTools, setBlacklistedTools] = useState<string[]>([]);
  const [newBlacklistTool, setNewBlacklistTool] = useState("");
  const [initialScore, setInitialScore] = useState(10);
  const [reflectionThreshold, setReflectionThreshold] = useState(5);
  // ── General / runtime settings (per-room RoomSettings) ─────────
  // Hydrated from `getGroupChatRoomSettings` on mount + every time
  // the drawer re-opens for a different room. `null` means "loading
  // or not yet fetched" — we render the controls in that state but
  // they're disabled, so the user can't save stale values.
  const [allowIdleChat, setAllowIdleChat] = useState(false);
  const [idleIntervalSecs, setIdleIntervalSecs] = useState(300);
  const [maxConcurrentAgents, setMaxConcurrentAgents] = useState(5);
  const [requireLeaderApproval, setRequireLeaderApproval] = useState(true);
  const [settingsLoaded, setSettingsLoaded] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveMsg, setSaveMsg] = useState("");

  const loadRules = useCallback(async () => {
    try {
      // F4 — single-room lookup. Previously this called
      // `listGroupChatRooms()` and `.find()`d the row, which paid the
      // cost of fetching every other room + their member profiles just
      // to read one room's rules. `GET /api/groupchat/rooms/{id}` is the
      // dedicated single-room endpoint and returns 404 (mapped to `null`)
      // when the room was deleted underneath the open drawer.
      const room = await api.getGroupChatRoom(roomId);
      if (room) {
        // Rules would come from the room's rules_json.
        // For now, load defaults.
      }
    } catch (e) {
      console.error("Failed to load rules:", e);
      toastError("加载规则失败");
    }
  }, [roomId]);

  useEffect(() => { loadRules(); }, [loadRules]);

  const loadSettings = useCallback(async () => {
    try {
      const s = await api.getGroupChatRoomSettings(roomId);
      setAllowIdleChat(s.allow_idle_chat);
      setIdleIntervalSecs(s.idle_interval_secs);
      setMaxConcurrentAgents(s.max_concurrent_agents);
      setRequireLeaderApproval(s.require_leader_approval);
      setSettingsLoaded(true);
    } catch (e) {
      // GET failures usually mean the room was deleted out from under
      // us. We keep whatever values the user previously had so the
      // drawer is still usable if they cancel.
      console.error("Failed to load room settings:", e);
toastError("加载聊天室设置失败");
      setSettingsLoaded(true);
    }
  }, [roomId]);

  useEffect(() => { loadSettings(); }, [loadSettings]);

  const handleSave = async () => {
    setSaving(true);
    setSaveMsg("");
    try {
      await api.updateGroupChatRules(roomId, {
        global_constraints: globalConstraints,
        tool_policy: {
          mode: toolPolicyMode,
          blacklisted: blacklistedTools,
          whitelisted: [],
        },
        penalty_policy: {
          initial_score: initialScore,
          reflection_threshold: reflectionThreshold,
          can_fire: true,
          auto_rebuild: true,
        },
      });
      setSaveMsg("规则已保存");
      safeSetTimeout(() => setSaveMsg(""), 2000);
    } catch (e) {
      setSaveMsg("保存失败");
    } finally {
      setSaving(false);
    }
  };

  // General tab save — persists all 4 runtime settings in one PUT.
  // The server's `update_settings` is partial-friendly: only the keys
  // we send are updated, so we always send the full snapshot for
  // simplicity. The idle timer is restarted/stopped server-side as a
  // side effect of the PUT, so we don't need to do anything here.
  const handleSaveSettings = async () => {
    setSaving(true);
    setSaveMsg("");
    try {
      await api.updateGroupChatRoomSettings(roomId, {
        allow_idle_chat: allowIdleChat,
        idle_interval_secs: idleIntervalSecs,
        max_concurrent_agents: maxConcurrentAgents,
        require_leader_approval: requireLeaderApproval,
      });
      setSaveMsg(allowIdleChat ? "已开启闲聊" : "设置已保存");
      safeSetTimeout(() => setSaveMsg(""), 2000);
    } catch (e) {
setSaveMsg("保存失败");
console.error("Failed to save room settings:", e);
toastError("保存聊天室设置失败");
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center p-4" style={{ background: "var(--color-overlay-scrim)", contain: "layout style paint" }} onClick={onClose}>
      <div
        onClick={(e) => e.stopPropagation()}
        className="w-full max-w-lg max-h-[80vh] rounded-2xl bg-bg-elevated border border-border-primary shadow-pop overflow-hidden flex flex-col"
        style={{ transform: "translateZ(0)" }}
      >
        {/* Header */}
        <div className="flex items-center justify-between px-4 py-3 bg-bg-secondary">
          <div className="flex items-center gap-2">
            <Settings size={16} className="text-text-secondary" />
            <h3 className="text-[14px] font-semibold text-text-primary">{roomName} — 规则设置</h3>
          </div>
          <button onClick={onClose} aria-label="关闭" className="p-1 rounded-md hover:bg-bg-hover text-text-muted">
            <X size={16} />
          </button>
        </div>

        {/* Tabs */}
        <div className="flex bg-bg-secondary">
          {(["rules", "general"] as const).map((t) => (
            <button
              key={t}
              onClick={() => setTab(t)}
              className={clsx(
                "px-4 py-2 text-[12.5px] font-medium transition-colors",
                tab === t ? "text-accent border-b-2 border-accent" : "text-text-muted hover:text-text-primary"
              )}
            >
              {t === "rules" ? "规则" : "通用"}
            </button>
          ))}
        </div>

        {/* Content */}
        <div className="flex-1 overflow-y-auto p-4 space-y-4">
          {tab === "rules" && (
            <>
              {/* Built-in rules (read-only) */}
              <div>
                <div className="flex items-center gap-1.5 mb-2">
                  <Shield size={13} className="text-accent" />
                  <span className="text-[13px] font-semibold text-text-primary">内置安全规则</span>
                  <span className="text-[12px] text-text-faint">（不可修改）</span>
                </div>
                <div className="rounded-lg bg-bg-tertiary p-3 space-y-1">
                  {["禁止执行破坏性命令", "禁止格式化磁盘", "禁止修改系统关键文件", "禁止未授权网络扫描"].map((rule) => (
                    <div key={rule} className="text-[13px] text-text-secondary flex items-center gap-1.5">
                      <span className="text-text-faint">•</span> {rule}
                    </div>
                  ))}
                </div>
              </div>

              {/* Global constraints */}
              <div>
                <div className="flex items-center gap-1.5 mb-2">
                  <AlertTriangle size={13} className="text-warning" />
                  <span className="text-[13px] font-semibold text-text-primary">全局规则</span>
                  <span className="text-[12px] text-text-faint">（适用于所有专家）</span>
                </div>
                <div className="space-y-1.5">
                  {globalConstraints.map((c, i) => (
                    <div key={`global-${i}`} className="flex items-center gap-2 bg-bg-tertiary rounded-lg px-3 py-1.5">
                      <span className="flex-1 text-[13px] text-text-secondary">{c}</span>
                      <button
                        onClick={() => setGlobalConstraints((prev) => prev.filter((_, j) => j !== i))}
                        className="text-text-faint hover:text-error text-[12px]"
                      >删除</button>
                    </div>
                  ))}
                  <div className="flex gap-1.5">
                    <input
                      value={newConstraint}
                      onChange={(e) => setNewConstraint(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter" && newConstraint.trim()) {
                          setGlobalConstraints((prev) => [...prev, newConstraint.trim()]);
                          setNewConstraint("");
                        }
                      }}
                      placeholder="添加新规则..."
                      className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[13px] text-text-primary placeholder:text-text-faint outline-none"
                    />
                    <button
                      onClick={() => {
                        if (newConstraint.trim()) {
                          setGlobalConstraints((prev) => [...prev, newConstraint.trim()]);
                          setNewConstraint("");
                        }
                      }}
                      disabled={!newConstraint.trim()}
                      className="px-2.5 py-1.5 rounded-lg text-[13px] font-medium disabled:opacity-40"
                      style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                    >添加</button>
                  </div>
                </div>
              </div>

              {/* Tool policy */}
              <div>
                <span className="text-[13px] font-semibold text-text-primary block mb-2">工具策略</span>
                <div className="space-y-2">
                  {(["blacklist", "whitelist", "strict"] as const).map((mode) => (
                    <label key={mode} className="flex items-center gap-2 cursor-pointer">
                      <input
                        type="radio"
                        name="toolPolicy"
                        checked={toolPolicyMode === mode}
                        onChange={() => setToolPolicyMode(mode)}
                        className="accent-[var(--color-accent)]"
                      />
                      <span className="text-[13px] text-text-secondary">
                        {mode === "blacklist" ? "黑名单模式（默认）" : mode === "whitelist" ? "白名单模式" : "严格模式"}
                      </span>
                    </label>
                  ))}
                </div>
                {toolPolicyMode === "blacklist" && (
                  <div className="mt-2 space-y-1.5">
                    {blacklistedTools.map((tool, i) => (
                      <div key={`bl-${tool}-${i}`} className="flex items-center gap-2 bg-bg-tertiary rounded-lg px-3 py-1.5">
                        <span className="flex-1 text-[13px] text-text-secondary font-mono">{tool}</span>
                        <button
                          onClick={() => setBlacklistedTools((prev) => prev.filter((_, j) => j !== i))}
                          className="text-text-faint hover:text-error text-[12px]"
                        >删除</button>
                      </div>
                    ))}
                    <div className="flex gap-1.5">
                      <input
                        value={newBlacklistTool}
                        onChange={(e) => setNewBlacklistTool(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === "Enter" && newBlacklistTool.trim()) {
                            setBlacklistedTools((prev) => [...prev, newBlacklistTool.trim()]);
                            setNewBlacklistTool("");
                          }
                        }}
                        placeholder="工具名称（如 terminal）..."
                        className="flex-1 rounded-lg bg-bg-tertiary px-3 py-1.5 text-[13px] text-text-primary font-mono placeholder:text-text-faint outline-none"
                      />
                      <button
                        onClick={() => {
                          if (newBlacklistTool.trim()) {
                            setBlacklistedTools((prev) => [...prev, newBlacklistTool.trim()]);
                            setNewBlacklistTool("");
                          }
                        }}
                        disabled={!newBlacklistTool.trim()}
                        className="px-2.5 py-1.5 rounded-lg text-[13px] font-medium disabled:opacity-40"
                        style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
                      >添加</button>
                    </div>
                  </div>
                )}
              </div>

              {/* Penalty policy */}
              <div>
                <div className="flex items-center gap-1.5 mb-2">
                  <Star size={13} className="text-warning" />
                  <span className="text-[13px] font-semibold text-text-primary">惩处策略</span>
                </div>
                <div className="grid grid-cols-2 gap-3">
                  <div>
                    <label className="text-[12px] text-text-faint block mb-1">初始积分</label>
                    <input
                      type="number"
                      value={initialScore}
                      onChange={(e) => setInitialScore(Number(e.target.value))}
                      className="w-full rounded-lg bg-bg-tertiary px-3 py-1.5 text-[13px] text-text-primary outline-none"
                    />
                  </div>
                  <div>
                    <label className="text-[12px] text-text-faint block mb-1">反思阈值</label>
                    <input
                      type="number"
                      value={reflectionThreshold}
                      onChange={(e) => setReflectionThreshold(Number(e.target.value))}
                      className="w-full rounded-lg bg-bg-tertiary px-3 py-1.5 text-[13px] text-text-primary outline-none"
                    />
                  </div>
                </div>
              </div>
            </>
          )}

          {tab === "general" && (
            <div className="space-y-4">
              {/* ── Idle chat (闲聊) ──────────────────────────── */}
              <div className="rounded-xl bg-bg-tertiary p-3 space-y-3">
                <label className="flex items-start gap-3 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={allowIdleChat}
                    onChange={(e) => setAllowIdleChat(e.target.checked)}
                    disabled={!settingsLoaded}
                    className="mt-0.5 accent-[var(--color-accent)]"
                    data-testid="toggle-idle-chat"
                  />
                  <div className="flex-1">
                    <div className="flex items-center gap-1.5 text-[13px] font-semibold text-text-primary">
                      <MessageCircle size={13} className="text-accent" />
                      启用闲聊模式
                    </div>
                    <div className="text-[12px] text-text-muted mt-0.5">
                      开启后,空闲的成员会按设定间隔自动发起分享经验 / 总结任务 / 学习心得的短消息。
                      关闭后定时器立即停止,不再产生闲聊消息。
                    </div>
                  </div>
                </label>

                <div className={clsx(
                  "pl-7 space-y-2 transition-opacity",
                  !allowIdleChat && "opacity-40 pointer-events-none"
                )}>
                  <div className="flex items-center gap-3">
                    <Clock size={12} className="text-text-faint" />
                    <label className="text-[13px] text-text-secondary whitespace-nowrap">
                      闲聊间隔 (秒)
                    </label>
                    <input
                      type="number"
                      min={30}
                      max={3600}
                      value={idleIntervalSecs}
                      onChange={(e) => setIdleIntervalSecs(Math.max(30, Number(e.target.value) || 30))}
                      disabled={!settingsLoaded || !allowIdleChat}
                      className="flex-1 rounded-lg bg-bg-secondary px-2.5 py-1 text-[13px] text-text-primary outline-none focus:ring-1 focus:ring-accent"
                    />
                  </div>
                  <div className="text-[12px] text-text-faint pl-5">
                    建议 60-600 秒。太短会刷屏,太长看不到效果。
                  </div>
                </div>
              </div>

              {/* ── Concurrency ──────────────────────────────── */}
              <div className="rounded-xl bg-bg-tertiary p-3 space-y-2">
                <div className="flex items-center gap-1.5 text-[13px] font-semibold text-text-primary">
                  <Users size={13} className="text-accent" />
                  并发上限
                </div>
                <div className="text-[12px] text-text-muted">
                  单条用户消息触发总管拆解后, 同时被调用的成员数量上限。
                </div>
                <div className="flex items-center gap-3 pl-0.5">
                  <input
                    type="number"
                    min={1}
                    max={20}
                    value={maxConcurrentAgents}
                    onChange={(e) => setMaxConcurrentAgents(Math.max(1, Math.min(20, Number(e.target.value) || 1)))}
                    disabled={!settingsLoaded}
                    className="w-24 rounded-lg bg-bg-secondary px-2.5 py-1 text-[13px] text-text-primary outline-none focus:ring-1 focus:ring-accent"
                  />
                  <span className="text-[12px] text-text-faint">1-20</span>
                </div>
              </div>

              {/* ── Leader approval ──────────────────────────── */}
              <div className="rounded-xl bg-bg-tertiary p-3 space-y-2">
                <label className="flex items-start gap-3 cursor-pointer">
                  <input
                    type="checkbox"
                    checked={requireLeaderApproval}
                    onChange={(e) => setRequireLeaderApproval(e.target.checked)}
                    disabled={!settingsLoaded}
                    className="mt-0.5 accent-[var(--color-accent)]"
                  />
                  <div className="flex-1">
                    <div className="flex items-center gap-1.5 text-[13px] font-semibold text-text-primary">
                      <CheckCircle2 size={13} className="text-accent" />
                      需要总管审批
                    </div>
                    <div className="text-[12px] text-text-muted mt-0.5">
                      开启后,总管拆解出的子任务在派发给成员前需要总管确认。
                      关闭后直接派发,响应更快。
                    </div>
                  </div>
                </label>
              </div>

              {!settingsLoaded && (
                <div className="text-[12px] text-text-faint text-center py-2">
                  正在加载设置…
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-between px-4 py-3 bg-bg-secondary">
          <span className="text-[12px] text-success">{saveMsg}</span>
          <div className="flex gap-2">
            <button onClick={onClose} className="px-3 py-1.5 rounded-lg text-[13px] font-medium" style={{ background: "var(--color-btn-secondary-bg)", color: "var(--color-btn-secondary-text)" }}>取消</button>
            <button
              onClick={tab === "general" ? handleSaveSettings : handleSave}
              disabled={saving}
              className="px-3 py-1.5 rounded-lg text-[13px] font-medium disabled:opacity-50"
              style={{ background: "var(--color-btn-primary-bg)", color: "var(--color-btn-primary-text)" }}
            >
              {saving ? "保存中..." : "保存"}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
