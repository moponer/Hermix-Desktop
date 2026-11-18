import { useCallback, useEffect, useState } from "react";
import { Eye, EyeOff } from "lucide-react";
import { fetchJSON } from "../api";

/**
 * 识图插件开关（输入框上方的眼睛图标）。
 *
 * 两种模式互斥：
 *
 * - **开** —— 图片走插件的免费视觉链，返回文字描述。纯文本模型也能用。
 * - **关** —— 图片作为 `ContentPart::ImageUrl` 直接进上下文，**要求当前模型
 *   本身是多模态**。纯文本模型收不了像素。
 *
 * 所以关着的时候如果模型不支持看图，按钮不能只是「静默可用」：用户粘了图、
 * 发送失败，却不知道是自己选错了模型。这里直接把状态显示在按钮上——
 * 标题写明「当前模型不支持识图」，图标同时置灰，粘图前就能看出来。
 *
 * 状态是**会话级**的：不同对话可以一个用插件、一个用原生多模态。
 */
export interface VisionToggleProps {
  /** 当前会话 id。空字符串时按钮禁用（还没有会话可写）。 */
  sessionId: string;
  /** 当前选中模型的 `supports_vision`。`undefined` = 上游没说，按可能支持处理。 */
  supportsVision?: boolean;
  /** 容器窄时只留图标。 */
  compact?: boolean;
}

interface VisionState {
  enabled: boolean;
  modelAcceptsImages: boolean;
}

export function VisionToggle({ sessionId, supportsVision, compact }: VisionToggleProps) {
  const [state, setState] = useState<VisionState>({
    enabled: false,
    // 未知能力不当作"不支持"——上游常省略该字段，前端替它拒绝会挡掉可用配置。
    modelAcceptsImages: supportsVision !== false,
  });
  const [busy, setBusy] = useState(false);

  // 切换会话 / 切换模型时重新读一次：开关是会话级，能力是模型级。
  useEffect(() => {
    if (!sessionId) return;
    let alive = true;
    const qs = new URLSearchParams({ session_id: sessionId });
    if (supportsVision !== undefined) qs.set("supports_vision", String(supportsVision));
    fetchJSON<VisionState>(`/api/vision-plugin?${qs.toString()}`)
      .then((r) => {
        if (alive) setState(r);
      })
      .catch(() => {
        // 读不到就保持默认，绝不让一个开关挡住输入框。
      });
    return () => {
      alive = false;
    };
  }, [sessionId, supportsVision]);

  const toggle = useCallback(async () => {
    if (!sessionId || busy) return;
    const next = !state.enabled;
    setBusy(true);
    // 乐观更新：眼睛必须立刻反应，否则点击像是没生效。
    setState((s) => ({ ...s, enabled: next }));
    try {
      const r = await fetchJSON<VisionState>("/api/vision-plugin", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({
          session_id: sessionId,
          enabled: next,
          supports_vision: supportsVision ?? null,
        }),
      });
      setState(r);
    } catch {
      setState((s) => ({ ...s, enabled: !next })); // 回滚
    } finally {
      setBusy(false);
    }
  }, [sessionId, busy, state.enabled, supportsVision]);

  // 关闭插件 + 模型明确不支持看图 = 这条路走不通。
  const blocked = !state.enabled && !state.modelAcceptsImages;

  const title = !sessionId
    ? "开始对话后可切换识图方式"
    : blocked
      ? "当前模型不支持识图：开启插件（走免费视觉链），或换一个多模态模型"
      : state.enabled
        ? "识图插件：已开启（图片经免费视觉链转成文字，任意模型可用）· 点击关闭"
        : "识图插件：已关闭（图片直接交给当前模型，需要多模态）· 点击开启";

  return (
    <button
      type="button"
      onClick={toggle}
      disabled={!sessionId || busy}
      title={title}
      aria-pressed={state.enabled}
      aria-label={state.enabled ? "关闭识图插件" : "开启识图插件"}
      className={`composer-chip ${compact ? "composer-chip--compact" : ""} ${
        state.enabled ? "composer-chip--on" : ""
      } ${blocked ? "text-[color:var(--color-text-warning)]" : ""} ${
        !sessionId ? "cursor-not-allowed opacity-50" : "cursor-pointer"
      }`}
    >
      {state.enabled ? <Eye /> : <EyeOff />}
      <span>{state.enabled ? "识图" : "识图关"}</span>
    </button>
  );
}
