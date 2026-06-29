import { Sparkles } from "lucide-react";

/**
 * H3: 实时意识流 (ThoughtStreamCard) — 心电图风格的活动指示器
 *
 * 设计目标：在 LLM 思考阶段（无 chunk 抵达）持续显示"agent is alive"
 * 的视觉信号。比起静态的"正在思考…"文字，心电图式的扫描线 + 跳动
 * 心率点更具现场感，使用户感到 agent 是真的在实时工作，而不是
 * 卡住了。
 *
 * 实现要点：
 *  1. **心率点** — 左侧绿色 dot，1.4s 心跳缩放（h-thought-pulse）
 *  2. **EKG 扫描线** — 60px 一周期的 P-QRS-T 波形，平移 -60px
 *     形成无缝右→左扫描错觉（h-thought-ekg-scroll）
 *  3. **可选 hint** — 简短的当前活动描述（"正在分析…"、"正在搜索…"）
 *  4. **可选 thoughts** — 最近 N 条"思维事件"小徽章（intent 识别、
 *     工具调用、阶段切换），让用户能扫一眼就知道 agent 在做什么
 *
 * 不会阻塞 UI：所有动画都是 CSS keyframes + transform，无 JS 计时。
 *
 * @see MessageBubble.tsx — `ThoughtStreamCard` 也在那里 re-export，
 *      保持与 `PrefetchIntentChip` 一致的导入路径。
 */

interface ThoughtEvent {
  /** 显示在徽章上的短文本，例如 "意图识别"、"工具调用"、"生成中" */
  label: string;
  /** 视觉权重：低=灰色、中=蓝、高=绿，影响颜色和顺序 */
  kind?: "info" | "tool" | "intent";
  /** 用于 React key；同一个 label 多次出现时必须不同 */
  key: string;
}

interface ThoughtStreamCardProps {
  /** 当前活动描述，例如 "正在分析…" */
  hint?: string;
  /**
   * 最近若干条"思维事件"，会渲染成右侧的小徽章列表。
   * 当 hint 不为空时，徽章会换到 hint 下方一行；为空时与心电图同排。
   */
  thoughts?: ThoughtEvent[];
  /** 包裹元素的自定义 className */
  className?: string;
  /**
   * 显示为"安静"模式：心率点变灰、波形透明度降低。
   * 用于 agent 完成所有推理但还在等最后批次的场景。
   */
  quiet?: boolean;
}

const KIND_CLASS: Record<NonNullable<ThoughtEvent["kind"]>, string> = {
  info: "thought-stream-event-info",
  tool: "thought-stream-event-tool",
  intent: "thought-stream-event-intent",
};

export function ThoughtStreamCard({
  hint,
  thoughts,
  className,
  quiet = false,
}: ThoughtStreamCardProps) {
  const hasEvents = thoughts && thoughts.length > 0;
  return (
    <div
      className={`thought-stream-card h-enter ${quiet ? "thought-stream-quiet" : ""} ${className ?? ""}`}
      role="status"
      aria-live="polite"
    >
      <div className="thought-stream-monitor">
        <span className="thought-stream-dot" aria-hidden="true" />
        <EkgTrace />
      </div>
      {hint && (
        <span className="thought-stream-hint">
          {hint}
        </span>
      )}
      {hasEvents && (
        <div className="thought-stream-events" aria-label="recent thoughts">
          {thoughts!.map((ev) => (
            <span
              key={ev.key}
              className={`thought-stream-event ${KIND_CLASS[ev.kind ?? "info"]}`}
            >
              {ev.kind === "intent" && <Sparkles size={9} aria-hidden="true" />}
              {ev.label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}

/**
 * 60px 一周期的 EKG 波形，在 viewBox 240×24 内重复 4 段。整条 path
 * 以 -60px 持续平移（h-thought-ekg-scroll），看上去就是"扫描"，
 * 与真实心电监护仪的视觉直觉一致。
 *
 * 周期内的小波 → 中等波 → 大尖峰模拟 P-QRS-T 复合波，但都是
 * 装饰性 — 真正要传达的信息只有"agent is alive"一件事。
 */
function EkgTrace() {
  return (
    <svg
      viewBox="0 0 240 24"
      className="thought-stream-ekg"
      preserveAspectRatio="none"
      aria-hidden="true"
    >
      <path
        className="thought-stream-ekg-path"
        fill="none"
        strokeWidth="1.4"
        strokeLinejoin="round"
        strokeLinecap="round"
        d="
          M 0 12
          H 10
          L 14 8 L 18 12
          H 22
          L 26 3 L 30 21 L 34 12
          H 38
          L 42 7 L 46 12
          H 60

          H 70
          L 74 8 L 78 12
          H 82
          L 86 3 L 90 21 L 94 12
          H 98
          L 102 7 L 106 12
          H 120

          H 130
          L 134 8 L 138 12
          H 142
          L 146 3 L 150 21 L 154 12
          H 158
          L 162 7 L 166 12
          H 180

          H 190
          L 194 8 L 198 12
          H 202
          L 206 3 L 210 21 L 214 12
          H 218
          L 222 7 L 226 12
          H 240
        "
      />
    </svg>
  );
}
