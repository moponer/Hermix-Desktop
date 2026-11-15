/**
 * 阿黎的角色形象 ——「墨团」。
 *
 * 为什么是墨团而不是动物：应用里已经有一只戴耳机的柴犬 mascot（public/mascot-*.png），
 * 阿黎是用户的「分身」，不该是第二只宠物。取「黎」的字义（黎明/将明未明），
 * 做成一滴悬浮的墨：没有手脚，靠姿态与轨道环表达状态，20px 与 120px 都认得出。
 *
 * 状态驱动，不做装饰性循环：
 *   idle     静候 —— 双眼平视，轨道环稳定
 *   thinking 在算 —— 左眼上有道轻抬的眉，眼睛略眯
 *   running  在跑 —— 双眼聚焦，右侧两道推进箭头
 *   done     完成 —— 双眼成笑弧
 *   alert    异常 —— 双眼睁圆，头顶一个叹号刻度
 *
 * 颜色一律走 CSS 变量（见 components-twin.css 的 .ali 段），
 * 组件内不出现任何字面色值，浅色/深色自动跟随主题。
 */
import { cx } from "../../lib/cx";

export type AliState = "idle" | "thinking" | "running" | "done" | "alert";

interface AliCharacterProps {
  state?: AliState;
  /** 渲染尺寸（px）。24–32 给顶栏，44–56 给在场位，96+ 给空态。 */
  size?: number;
  className?: string;
  /** 角色名，供读屏使用；纯装饰调用时传空串。 */
  label?: string;
}

export function AliCharacter({
  state = "idle",
  size = 48,
  className,
  label = "助手阿黎",
}: AliCharacterProps) {
  return (
    <span
      className={cx("ali", className)}
      data-state={state}
      role={label ? "img" : undefined}
      aria-label={label || undefined}
      aria-hidden={label ? undefined : true}
    >
      <svg
        viewBox="0 0 96 96"
        width={size}
        height={size}
        focusable="false"
        aria-hidden
      >
        {/* 轨道环：她「一直在看着」的记号，断开的一小段是刻度 */}
        <ellipse className="ali__orbit" cx="48" cy="50" rx="44" ry="30" />
        <path className="ali__orbit-tick" d="M92 50a44 30 0 0 1-12 21" />

        {/* 本体与两只小角 */}
        <g className="ali__figure">
          <path
            className="ali__body"
            d="M48 16c21 0 34 15 34 33 0 19-15 33-34 33S14 68 14 49c0-18 13-33 34-33Z"
          />
          <path className="ali__body" d="M33 19c-4-6-1-11 4-11s7 5 3 11" />
          <path className="ali__body" d="M63 19c-4-6-1-11 4-11s7 5 3 11" />

          {/* 双眼（完成态换成笑弧） */}
          <g className="ali__eyes-dot">
            <circle cx="39" cy="50" r="4.2" />
            <circle cx="57" cy="50" r="4.2" />
          </g>
          <g className="ali__eyes-happy">
            <path d="M34 51c2-4 6-4 8 0" />
            <path d="M54 51c2-4 6-4 8 0" />
          </g>

          {/* 状态标记 */}
          <path className="ali__mark ali__mark--thinking" d="M34 41h10" />
          <g className="ali__mark ali__mark--running">
            <path d="M70 44l5 5-5 5" />
            <path d="M78 44l5 5-5 5" />
          </g>
          <g className="ali__mark ali__mark--alert">
            <path d="M48 22v8" />
            <circle cx="48" cy="35" r="1.8" />
          </g>
        </g>
      </svg>
    </span>
  );
}
