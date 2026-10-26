import type { ReactNode } from "react";

import { cx } from "../../lib/cx";
import { type IconTone } from "../../lib/categoryTone";

/**
 * 设置页的选择卡（外观页明暗三选、通用页工作模式二选共用）。
 *
 * 为什么抽成共享件：两处卡片的需求完全一样（图标 + 标题 + 说明 + 选中态），
 * 各自写一份必然漂移——上次就是两边都"没有层次、图标没颜色"，
 * 因为它们都是随手写的默认排版。收敛到一处后改一次两处都生效。
 *
 * 层次由四层承担，从强到弱：
 *   1. 图标色块（唯一的彩色，视线第一落点）
 *   2. 标题（正文级字号 + primary 色）
 *   3. 说明（meta 字号 + muted 色，允许两行）
 *   4. 选中指示（底色加深一档 + 单选环填充）—— 不用描边
 * 靠"一彩三灰"的对比建立主次，而不是给每层都上色。
 */
export type CardTone = IconTone;

/**
 * 2026-10-23：这张表**已删除**（下方说明留档，别再照着恢复）。
 *
 * 旧版 `TONE` 给每个色调手写「柔和底 / 饱和图标」两支色值，然后用
 * `style={{ background: t.chip, color: t.icon }}` 涂到图标块上。三个问题：
 *   1. 底色是**手调的软值**（--color-success-soft 等），与图标色的浓淡关系
 *      靠肉眼对齐，换主题后必然失衡；
 *   2. 行内样式没有 :hover，图标块在整卡 hover 时毫无反馈；
 *   3. 同样的「柔和底 + 饱和图标」在 icon-tone.css 里已经有一份实现
 *      （14% color-mix 现算），两份并存就是漂移的开始。
 *
 * 现在改为 `cat-tile--${tone}` 一个类：底是身份色实底，字是反白前景
 * （`--color-on-tone`，分主题：浅色白字 / 深色深墨）。
 *
 * ## 色调从语义四色改成身份色（2026-10-23 二次修正）
 *
 * 此前这里用 success / warning / info 给「浅色 / 深色 / 跟随系统」上色，
 * 违反本项目自己的纪律：**语义四色表达「状态」，身份色表达「是什么」**。
 * 一张选择卡不是状态，是选项 —— 用 success 表示"跟随系统"更是把
 * "跟随"读成了"成功"。现在统一走 8 档身份色：
 * 浅色=amber（暖=亮）· 深色=violet（夜）· 跟随系统=slate（中性=跟随）
 * 编程开发=blue（与 categoryTone 的「开发/运维=blue」一致）
 * 日常办公=green（与「办公=green」一致）。
 *
 * 顺带解掉一个对比度死结：语义四色做实底时，白字在暗色主题只有
 * 2.30~3.28:1（不达 3:1），在 ink-green 的 warning 上只有 2.46:1；
 * 身份色 8 档在两套主题下白字全部达标（浅色 3.17~4.82 / 深色改深墨
 * 6.99~9.14）。
 */
export interface ChoiceCardProps {
  label: string;
  /** 可省略：选项名本身已自明时不要补一句解释（见 AGENTS.md 界面审查先做减法）。 */
  description?: string;
  icon: ReactNode;
  tone: CardTone;
  selected: boolean;
  onSelect: () => void;
  /** 次要标注（如「跟随系统」当前解析成深色）。 */
  meta?: string;
  disabled?: boolean;
}

export function ChoiceCard({
  label, description, icon, tone, selected, onSelect, meta, disabled,
}: ChoiceCardProps) {
  return (
    <button
      type="button"
      role="radio"
      aria-checked={selected}
      disabled={disabled}
      onClick={onSelect}
      className={cx(
        "flex w-full flex-col items-start gap-2.5 rounded-[var(--radius-lg)] p-4 text-left transition-colors",
        "shadow-[var(--shadow-paper)]",
        /* 焦点：不用 ring（那是一圈描边）。键盘焦点由 base.css 的全局
           :focus-visible 淡晕承担，鼠标点击则完全不留痕迹。 */
        "disabled:cursor-not-allowed disabled:opacity-50",
        /* 选中态：靠**底色浓度**而不是描边 —— accent-soft-strong(18%) 比
           accent-soft(10%) 明显更深，与未选中的卡片底拉开两档。 */
        selected
          ? "bg-[var(--color-accent-soft-strong)]"
          : "bg-[var(--color-bg-card)] hover:bg-[var(--color-bg-card-hover)]",
      )}
    >
      <div className="flex w-full items-center gap-2.5">
        {/* 图标块走共享的 cat-tile（身份色实底 + 反白字形），不再用行内
            样式写死配色 —— 行内样式没有 :hover，且改色要逐处改。 */}
        <span
          aria-hidden
          className={cx("cat-tile cat-tile--md", `cat-tile--${tone}`)}
        >
          {icon}
        </span>
        <span className="min-w-0 flex-1">
          <span
            className={cx(
              "block text-[length:var(--text-body)] font-medium leading-tight",
              selected ? "text-accent" : "text-text-primary",
            )}
          >
            {label}
          </span>
          {meta && (
            <span className="mt-0.5 block text-[length:var(--text-tiny)] leading-tight text-[var(--color-text-muted)]">
              {meta}
            </span>
          )}
        </span>
        {/* 单选环：色觉障碍下靠形状也能判选中，不只靠颜色 */}
        {/* 单选环：无描边。未选中是「淡染底的空心圆」（bg-bg-tertiary），
            选中是「accent 实心底 + 白勾」。两态差的是**填充浓度**而不是
            有没有框 —— 去掉 2px 边框后，环在扁平界面上不再长出一层壳。
            形状本身（圆 + 勾）在色觉障碍下仍可读，不只靠颜色。 */}
        <span
          aria-hidden
          className={cx(
            "flex h-4 w-4 shrink-0 items-center justify-center rounded-full transition-colors",
            selected ? "bg-accent text-on-accent" : "bg-bg-tertiary text-transparent",
          )}
        >
          {selected && (
            <svg width="10" height="10" viewBox="0 0 10 10" fill="none">
              <path
                d="M2 5l2 2 4-4"
                stroke="currentColor"
                strokeWidth="1.8"
                strokeLinecap="round"
                strokeLinejoin="round"
              />
            </svg>
          )}
        </span>
      </div>

      {description && (
        <span className="text-[length:var(--text-ui)] leading-relaxed text-[var(--color-text-muted)]">
          {description}
        </span>
      )}
    </button>
  );
}
