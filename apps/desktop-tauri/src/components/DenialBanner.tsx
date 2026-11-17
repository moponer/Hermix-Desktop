/**
 * 工具调用被拒绝/超时/断连后的非阻塞提示条（从 App.tsx 的局部 IIFE 抽出，H7）。
 * 只负责展示：出现与否由 App 的 denialBanner state 决定，30s 自动消失也由 App 管理。
 * `policy` 类静默拦截不会走到这里（App 在 showDenialBanner 前已过滤）。
 */
import { CLOSE } from "./strings";

export interface DenialBannerInfo {
  kind: string;
  tool: string;
}

/** kind → 副文案。单独导出以便不渲染即可锁定全部分支。 */
export function denialSubtitle(kind: string): string {
  if (kind === "user_denied") return "你明确拒绝了该操作，未执行。";
  if (kind === "timed_out") return "确认超时（你在限定时间内没有做出决定），未执行。";
  if (kind === "channel_closed") return "连接中断（WS 断开 / 服务端重启 / 会话重载），请求丢失。";
  return "未执行。";
}

export function DenialBanner({
  denial,
  onDismiss,
}: {
  denial: DenialBannerInfo | null;
  onDismiss: () => void;
}) {
  if (!denial) return null;
  return (
    <div
      className="denial-banner"
      role="status"
      aria-live="polite"
      onClick={onDismiss}
    >
      <span className="denial-banner__icon">!</span>
      <div className="denial-banner__body">
        <div className="denial-banner__title">
          工具 <code>{denial.tool}</code> 的调用被拒绝
        </div>
        <div className="denial-banner__sub">{denialSubtitle(denial.kind)}</div>
      </div>
      <button
        className="denial-banner__close"
        aria-label={CLOSE}
        onClick={(e) => { e.stopPropagation(); onDismiss(); }}
      >×</button>
    </div>
  );
}
