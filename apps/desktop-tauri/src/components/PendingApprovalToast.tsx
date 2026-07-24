import { AlertCircle, X } from "lucide-react";
import { useEffect, useState } from "react";

/**
 * PendingApprovalToast
 * ─────────────────────────────────────────────────────────────────
 * 右下角气泡 — 用户 反馈"差点没看见"后加的提醒机制。
 *
 * 为什么需要:
 *   · `InlineApprovalCard` 嵌在聊天流中间位置。用户如果:
 *       (a) 在看别人的回复 / 在 Reading Sidebar 看 artifact
 *       (b) 切到其他 session 找了东西
 *       (c) 单纯的 chrome alt-tab 走了
 *     都有可能漏看。一句 agent 的 "正在思考" 也会盖在卡片上面,
 *     让卡片短暂不可见。**任何这种"看不见 = 漏掉决定"都是产品 bug**。
 *   · 气泡提供视觉冗余:即使卡片在主区看不见,右下角的气泡
 *     永远在,数字 + 图标一秒就传达"Hermix 在等"语义。
 *
 * 行为:
 *   · `count > 0` 时显示;`count === 0` 时整组件不渲染(避免
 *     长时间积累空 toast 占据 fixed 层)
 *   · 单击 toast → App.tsx 注入的 `onActivate` 回调(滚动并高亮
 *     第一张未决定卡)。**注意:点击的不是关闭,是切换视角。**
 *   · X 按钮只在 `dismissible=true` 时显示 — 用于"我已经看过了,
 *     让我安静做事"的场景,不算决定(超时仍会被自动拒绝)。
 *
 * Props:
 *   - count:  当前 pending 数(由 App.tsx 用 approvalResolversRef.size
 *             同步成 state 后传入)
 *   - urgent: 是否有任意卡片的剩余时间 ≤ 30s — 用于切换红框 pulse
 *   - onActivate: 单击气泡 → 滚动到最早未决定卡 + 2s 黄色高亮闪烁
 *   - onDismiss: 点 X → 隐藏直到下一张新卡到来(用户主动静默)
 *
 * 设计原则:
 *   · 永远不调用 onApprove/onDeny — 这是**提醒组件**,不是决策组件。
 *     用户必须回主聊天区看上下文、审 args、再点。这避免误触。
 *   · 体积小,圆角胶囊,跟 macOS / Windows 11 通知中心同一象限:
 *     bottom-right 24px,主角色 warning,urgent 时红。
 *   · 不依赖 portal — 已挂在 app-shell root,fixed 即可定位。
 *   · pre-existing 风格:不引入任何蓝/紫/靛;toast = 警告语义,用
 *     --color-warning / --color-error(琥珀 + 红),与已有 token 系统
 *     一致。
 */
export interface PendingApprovalToastProps {
  count: number;
  urgent?: boolean;
  onActivate: () => void;
  onDismiss?: () => void;
}

export function PendingApprovalToast({
  count,
  urgent = false,
  onActivate,
  onDismiss,
}: PendingApprovalToastProps) {
  const [dismissedKey, setDismissedKey] = useState<string | null>(null);

  /**
   * 当新一轮的 batch 到来,即使之前 dismiss 过,也应该重新冒头。
   * 用 `count` 变化 + "dismissed" 时显示的 count 作 key,匹配则保持隐藏。
   */
  useEffect(() => {
    if (count === 0) setDismissedKey(null);
  }, [count]);

  if (count <= 0) return null;
  const dismissKey = `count-${count}`;
  if (dismissedKey === dismissKey) return null;

  const label = count === 1 ? "请求等你确认" : `等 ${count} 项你确认`;
  const handleActivate = () => onActivate();
  const handleDismiss = () => {
    setDismissedKey(dismissKey);
    onDismiss?.();
  };

  return (
    <div
      role="button"
      tabIndex={0}
      className="pending-approval-toast"
      data-urgent={urgent || undefined}
      onClick={handleActivate}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          handleActivate();
        }
      }}
      aria-label={`${label}，点击跳到对话中的请求`}
      title="点击跳到对话中等待你确认的请求"
    >
      <span className="pending-approval-toast-icon" aria-hidden="true">
        <AlertCircle size={14} />
      </span>
      <span className="pending-approval-toast-label">
        <span className="pending-approval-toast-count">{count}</span>
        <span>{label}</span>
      </span>
      {onDismiss && (
        <button
          type="button"
          aria-label="关闭提醒(不会决定请求)"
          className="pending-approval-toast-close"
          onClick={(e) => {
            // Don't bubble — clicking X must NOT scroll/highlight the card.
            e.stopPropagation();
            handleDismiss();
          }}
          onKeyDown={(e) => e.stopPropagation()}
        >
          <X size={13} />
        </button>
      )}
    </div>
  );
}
