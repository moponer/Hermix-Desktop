/**
 * 阿黎的任务面板 —— 需求队列的纯逻辑（不碰 React、不碰网络）。
 *
 * 交互模型（2026-11-06 重构）：输入框不再是「跟她聊天」，而是**下达需求**。
 * 需求先入队（面板顺序 = 优先级），她定时从队首拉取几条开工。
 *
 * **这里不做任何"归类"。** 上一版按关键词把需求分成 学习/查询/整理…，
 * 再按类型合批 —— 用户当场指出这是错的：两条需求是不是同一件事，是语义
 * 判断，关键词表判不出来（"把课程刷完"和"把作业交了"都是学习，但未必
 * 该并成一条任务线；"整理笔记"和"写周报"分属两类，可能正是同一件事的
 * 两半）。归类交给**阿黎自己**：她在回复里为每条需求写出归档判断
 * （`【归档】<需求> → 并入|新开 <任务名>`），前端只负责把这段原文摘出来
 * 显示，不猜。
 *
 * 前端在这条链上只承担三件机械事：**排队、限量、送达**。
 */
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

/** 面板里的一条需求。`id` 稳定，排序/删除都认它。 */
export interface TwinTask {
  id: string;
  text: string;
  /** 入队时刻（ISO）。 */
  createdAt: string;
  /**
   * 被阿黎退回（缺目标、无法动手）。**blocked 的条目不会被自动拉取** ——
   * 否则 4s 一次的拉取会把她烧在「拉取 → 退回 → 再拉取」里。
   */
  blocked?: boolean;
  /** 退回原因（她的原话，形如「缺 目标文件路径」）。 */
  blockReason?: string;
  /** 累计被退回次数：到 `RETURN_LOCK` 次就只剩编辑或取消两条路。 */
  returns?: number;
}

/** 同一需求被退回这么多次后不再自动尝试（必须你编辑后重新提交）。 */
export const RETURN_LOCK = 3;

/** 一次拉取 = 这一轮交给她安排的需求。 */
export interface TwinBatch {
  id: string;
  tasks: TwinTask[];
  /** 本轮开始时刻（ms）——团队看板按它筛选「本批派出去的工单」。 */
  startedAt: number;
  status: "running" | "done" | "failed";
  /** 失败原因（status=failed 时非空，直接上屏，不写"见下方错误"）。 */
  error?: string;
  /** 失败后是否已把需求放回面板（避免重复入队）。 */
  requeued?: boolean;
}

/**
 * 一轮最多带几条。
 *
 * 这是**消息体量上限**，不是语义分组：再多的话模型容易只做前两条，或者
 * 回复被截断。同一件事要不要拆成多轮，由她判断，不由这个数字决定。
 */
export const BATCH_MAX = 6;
/** 面板容量上限：再多就不适合"手工排序"这种交互了。 */
export const QUEUE_MAX = 60;

let seq = 0;
/** 构造一条需求；`id` 用时间 + 自增，同一毫秒内批量入队也不会撞。 */
export function makeTask(text: string, now = Date.now()): TwinTask {
  seq = (seq + 1) % 10_000;
  return {
    id: `t${now.toString(36)}-${seq}`,
    text: text.trim(),
    createdAt: new Date(now).toISOString(),
  };
}

/** 入队（追加到队尾 = 最低优先级）。空文本/重复文本不入队。 */
export function enqueue(list: TwinTask[], task: TwinTask): TwinTask[] {
  if (!task.text) return list;
  if (list.some((t) => t.text === task.text)) return list;
  return [...list, task].slice(-QUEUE_MAX);
}

/** 上移/下移一位（`dir=-1` 提升优先级）。越界时返回原数组引用。 */
export function moveTask(list: TwinTask[], id: string, dir: -1 | 1): TwinTask[] {
  const i = list.findIndex((t) => t.id === id);
  if (i < 0) return list;
  const j = i + dir;
  if (j < 0 || j >= list.length) return list;
  const next = [...list];
  [next[i], next[j]] = [next[j], next[i]];
  return next;
}

/**
 * 从队首取一批（保持面板顺序，最多 `max` 条）。
 *
 * 不做任何合并：取哪几条只由**用户排的顺序**决定。是不是同一件事、该并进
 * 哪条任务线，全部留给阿黎在她那一轮里判断。
 *
 * **跳过 blocked**（等用户补充的）—— 它们留在面板里，但不参与自动拉取。
 */
export function takeBatch(
  list: TwinTask[],
  max = BATCH_MAX,
): { batch: TwinTask[]; rest: TwinTask[] } {
  const open = list.filter((t) => !t.blocked);
  if (open.length === 0) return { batch: [], rest: list };
  const batch = open.slice(0, Math.min(Math.max(max, 1), open.length));
  const taken = new Set(batch.map((t) => t.id));
  return { batch, rest: list.filter((t) => !taken.has(t.id)) };
}


/** 归档判断行的固定标记 —— 指令里写死，前端据此摘原文，不做语义猜测。 */
export const ARCHIVE_MARK = "【归档】";

/** 退回行的固定标记（缺目标、没动手的需求）。 */
export const RETURN_MARK = "【退回】";

/**
 * 把一批需求编译成一条下发消息。
 *
 * 四个刻意的设计：
 * ① **先判断再动手**：明确要求她拿这条需求去比「当前正在做的任务」和
 *    本批其它需求 —— 相关的并入同一任务线（沿用已派工单/成员，不重复
 *    开工），无关的另起；
 * ② **归档格式固定**：`【归档】<需求要点> → 并入|新开 <任务名>`，这样
 *    状态页展示的是她自己写的判断原文，而不是前端猜的分类；
 * ③ **没目标的退回**：`【退回】<需求要点> → 缺 <什么>`，前端据此把这条
 *    放回面板等你补充 —— 猜着做比不做更贵；
 * ④ **每条单独回报**：只给总体结论时，"哪条做完了"无从核对。
 */
export function batchPrompt(tasks: TwinTask[]): string {
  if (tasks.length === 0) return "";
  const lines = tasks.map((t, i) => `${i + 1}. ${t.text}`).join("\n");
  return [
    `【任务面板 · 本批 ${tasks.length} 条】`,
    lines,
    "",
    "先判断，再动手：",
    "- 逐条比较：这条需求跟你**当前正在做的任务**、以及本批其它需求，是不是同一件事；",
    "- 是 → 并入同一任务线：沿用已经派出去的工单/成员，不要重复开工、不要重建团队；",
    "- 不是 → 作为新任务另起一条线（确实需要人手再派团队）；",
    `- 每条先写一行归档，格式固定：\`${ARCHIVE_MARK}<需求要点> → 并入 <任务名>\` 或 \`${ARCHIVE_MARK}<需求要点> → 新开 <任务名>\`；`,
    `- **没目标就别猜**：某条需求缺了关键信息就没法动手时，不要执行它，写一行 \`${RETURN_MARK}<需求要点> → 缺 <什么>\`，其余照做；`,
    "- 能从对话上文 / 文件名 / 历史里合理推断的，按最省事的假设做并在归档里注明，不要为一件小事反复追问；",
    "- 按面板顺序处理，每条单独回报结果，失败的说明原因。",
  ].join("\n");
}

/** 从她的回复里摘出行首带某个标记的行（原文，不加解释）。 */
function linesWithMark(reply: string, mark: string, cap = 12): string[] {
  return reply
    .split(/\r?\n/)
    .map((line) => line.trim().replace(/^[-*+]\s+/, ""))
    .filter((line) => line.startsWith(mark))
    .map((line) => line.slice(mark.length).trim())
    .filter(Boolean)
    .slice(0, cap);
}

/**
 * 归档判断行。她要是没写，返回空数组，由界面如实说"没有给出归档说明"，
 * 而不是拿关键词反推一个分类糊上去。
 */
export function verdictLines(reply: string): string[] {
  return linesWithMark(reply, ARCHIVE_MARK);
}

/** 退回行（缺目标、没动手的那些需求）。 */
export function returnLines(reply: string): string[] {
  return linesWithMark(reply, RETURN_MARK);
}

/** 退回行 ↔ 需求 的对齐结果。 */
export interface ReturnedTask {
  task: TwinTask;
  reason: string;
}

/** 去掉空白与中英文标点后再比对（模型复述需求时几乎不会逐字一致）。 */
function normalized(s: string): string {
  return s
    .replace(/[\s，。、；：！？"'“”‘’（）()【】\[\]…·.,;:!?<>《》-]/g, "")
    .toLowerCase();
}

/**
 * 把「退回」行对回具体需求。
 *
 * 匹配策略（从严到宽，仍然只做字面比对，不做语义猜测）：
 * ① 行的前半段与需求正文互相包含（≥4 字，避免"整理"这种短词乱命中）；
 * ② 本批只有一条需求、且只有一行退回时，直接归给它（模型常只写要点）。
 * 对不上就留在 `unmatched` 里由界面照登 —— 宁可不处理，也不张冠李戴。
 */
export function matchReturned(
  tasks: TwinTask[],
  lines: string[],
): { matched: ReturnedTask[]; unmatched: string[] } {
  const matched: ReturnedTask[] = [];
  const unmatched: string[] = [];
  const used = new Set<string>();
  for (const line of lines) {
    const [subjectRaw, reasonRaw] = line.split(/\s*(?:→|->)\s*/, 2);
    const subject = normalized(subjectRaw ?? "");
    const reason = (reasonRaw ?? "").trim() || "目标不明确";
    let hit: TwinTask | undefined;
    if (subject.length >= 4) {
      hit = tasks.find((t) => {
        if (used.has(t.id)) return false;
        const body = normalized(t.text);
        return body.length >= 4 && (body.includes(subject) || subject.includes(body));
      });
    }
    if (!hit && tasks.length === 1 && lines.length === 1 && !used.has(tasks[0].id)) {
      hit = tasks[0];
    }
    if (hit) {
      used.add(hit.id);
      matched.push({ task: hit, reason });
    } else {
      unmatched.push(line);
    }
  }
  return { matched, unmatched };
}

// ── 落盘 ──────────────────────────────────────────────────────────────
// 只存"待拉取"的队列：批次是这一轮的内存态，落盘反而会让重启后出现一条
// 永远停在 running 的幽灵批次（真实流已经随窗口销毁结束了）。

export function loadQueue(): TwinTask[] {
  try {
    const raw = storage.getItem(STORAGE_KEYS.twinQueue);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter(
        (v): v is TwinTask =>
          !!v &&
          typeof (v as TwinTask).id === "string" &&
          typeof (v as TwinTask).text === "string" &&
          !!(v as TwinTask).text.trim(),
      )
      .map((t) => ({
        id: t.id,
        text: t.text,
        createdAt: typeof t.createdAt === "string" ? t.createdAt : "",
        // 退回状态要跨重启保留：否则重开窗口后它又会被自动拉走、再退一次。
        ...(t.blocked === true ? { blocked: true } : {}),
        ...(typeof t.blockReason === "string" ? { blockReason: t.blockReason } : {}),
        ...(typeof t.returns === "number" && t.returns > 0 ? { returns: t.returns } : {}),
      }))
      .slice(-QUEUE_MAX);
  } catch {
    return [];
  }
}

export function saveQueue(list: TwinTask[]): void {
  try {
    storage.setItem(STORAGE_KEYS.twinQueue, JSON.stringify(list));
  } catch {
    /* 存不下不影响本次使用：队列在内存里照常工作 */
  }
}
