/**
 * 读数格式化 —— 观测台语言里所有"数字怎么显示"的唯一出处。
 *
 * 为什么单独一个模块：值守页与任务台都要用同一套读数写法（定宽两位、
 * 紧凑相对时间），两处各写一份必然漂移（一处补零一处不补，同一屏里
 * `7` 和 `07` 并存，读数就"不像仪表"了）。
 *
 * 纪律：这两个函数只做**纯格式化**，不读 DOM、不读时间以外的状态，
 * 便于单测；时间来源一律 `Date.now()`，由调用方在渲染时传入 iso 字符串。
 */

/** 两位定宽读数（`00` / `07`）。宽度固定，值变化时行内不会抖。 */
export function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

/**
 * 紧凑相对时间：`刚` / `5m` / `18h` / `3d`；未来时间加 `+` 前缀。
 *
 * 中文写法「18 小时前」在 34~36px 的行首列里会折成两行（实测截图），
 * 紧凑写法既省宽也与读数语言一致；完整说法由调用方挂在 `title` 上。
 */
export function relativeCompact(iso: string | undefined | null): string {
  if (!iso) return "—";
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return "—";
  const diffMin = Math.round((Date.now() - t) / 60_000);
  const m = Math.abs(diffMin);
  const body =
    m < 1
      ? "刚"
      : m < 60
        ? `${m}m`
        : m < 60 * 24
          ? `${Math.round(m / 60)}h`
          : `${Math.round(m / (60 * 24))}d`;
  return diffMin < 0 ? `+${body}` : body;
}

/**
 * 「最近会话」行标题：库里没有标题时，说清楚这条会话是什么。
 *
 * `getSessions` 把 NULL 标题替换成字面量 `New chat`（routes.rs:273），而
 * 只有主对话走前端自动起名。值守页列的是阿黎工作域（`twin:` / `member::` /
 * `automation:`），这些会话永远拿不到标题 —— 用户看到的是两行一模一样的
 * `New chat`（2026-11-06 实报截图），分不清谁是谁。
 *
 * 三类来源都写在 id 上，按 id 归类即可；`automation:<taskId>:<runId>` 还能
 * 顺着 taskId 换回任务名。**不猜内容**：换不回来就说"自动化运行"。
 */
export function sessionRowLabel(
  id: string,
  title: string | null | undefined,
  automationName?: (automationId: string) => string | undefined,
): string {
  const t = (title ?? "").trim();
  if (t && t !== "New chat") return t;
  if (id.startsWith("twin:")) return "阿黎的对话";
  if (id.startsWith("member::")) return "成员工单";
  const auto = /^automation:([^:]+):/.exec(id);
  if (auto) {
    const name = automationName?.(auto[1]);
    return name ? `自动化 · ${name}` : "自动化运行";
  }
  return t || "未命名会话";
}

/**
 * 时钟读数（`14:03:41`）—— 心跳/巡查用它标"上次巡查是什么时候"。
 *
 * 用 `Date` 的本地时区（值守页面对的是人，不是 UTC 日志），时分秒定宽，
 * 数值跳变时不会抖。
 */
export function clockTime(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`;
}

/**
 * 工作日志的一行 → 可直接上屏的一句话。
 *
 * `daily_memory` 的 `conclusions` 存的是**写入格式**，不是显示格式：
 *
 * ```
 * - 14:32 [automati] USER: 把昨天那份周报转成 PDF 发我
 * ```
 *
 * 原样搬上屏会带出三样对用户无意义的东西：markdown 项目符号、与行首
 * 日期列重复的时刻、以及会话 id 的前 8 位（`[automati]` 是 `automation:…`
 * 被截断的产物，Rust 侧 `append_daily_memory_conclusion` 只取 8 字符）。
 * 这里逐段剥掉，只留结论本身；剥完是空的才退回原文（宁可带符号，不可空行）。
 */
export function dailyLine(text: string | undefined | null, max = 80): string {
  if (!text) return "";
  const raw =
    text
      .split(/\r?\n/)
      .map((s) => s.trim())
      .find((s) => s.length > 0) ?? "";
  if (!raw) return "";
  const body =
    raw
      .replace(/^[-*+]\s+/, "")
      .replace(/^\d{4}-\d{2}-\d{2}[ T]/, "")
      .replace(/^\d{1,2}:\d{2}(:\d{2})?\s*/, "")
      .replace(/^\[[^\]]{0,24}\]\s*/, "")
      .replace(/^(USER|ASSISTANT|SYSTEM|用户|助手)\s*[:：]\s*/i, "")
      .trim() || raw;
  return body.length > max ? `${body.slice(0, max - 1)}…` : body;
}
