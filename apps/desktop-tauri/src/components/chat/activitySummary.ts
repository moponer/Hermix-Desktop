import { useCallback, useEffect, useState } from "react";

import type { TimelineStep } from "../../types";

/**
 * 折叠组的摘要措辞与开合记忆。抄 cc-haha
 * `desktop/src/components/chat/activityGroupModel.ts` + `ActivityGroup.tsx`。
 *
 * 三个要点：
 *
 * 1. **按文件去重计数。** 同一个文件被改了 8 次，说「编辑了 8 个文件」是
 *    骗人的 —— `activityGroupModel.ts:98-108` 就是按 `file_path` 去重的。
 *
 * 2. **只调一次工具时不生成摘要。** `ActivityGroup.tsx:91-103`。理由在
 *    源码注释里：「Read 1 file」压在一个被藏起来的 Read 行上面，携带的
 *    信息严格少于那一行本身。
 *
 * 3. **开合状态要扛住虚拟化。** 消息列用了 `content-visibility: auto`，
 *    行滚出视口会被回收再挂载，`useState` 归零 —— 用户的展开选择会被无声
 *    吞掉。所以状态存在模块级 Map 里，key 是这一轮的稳定标识。
 */

/** 工具名 → 摘要动词。抄 cc-haha `TOOL_VERBS`。没有动词的工具保留自己的
 *  名字（`SendMessage (2)`），因为「用了哪个」才是有用的信息。 */
const TOOL_VERBS: Record<string, { one: string; many: (n: number) => string; unit: "file" | "call" }> = {
  read_file: { one: "读取了 1 个文件", many: (n) => `读取了 ${n} 个文件`, unit: "file" },
  write_file: { one: "创建了一个文件", many: (n) => `创建了 ${n} 个文件`, unit: "file" },
  patch: { one: "编辑了一个文件", many: (n) => `编辑了 ${n} 个文件`, unit: "file" },
  search_files: { one: "查找到文件", many: () => "查找到文件", unit: "file" },
  terminal: { one: "执行了一条命令", many: (n) => `执行了 ${n} 条命令`, unit: "call" },
  delegate_task: { one: "派遣了一个代理", many: (n) => `派遣了 ${n} 个代理`, unit: "call" },
  web_fetch: { one: "获取了一个页面", many: (n) => `获取了 ${n} 个页面`, unit: "call" },
  web_search: { one: "搜索了网页", many: () => "搜索了网页", unit: "call" },
};

function argPath(args: string | undefined): string | undefined {
  if (!args) return undefined;
  try {
    const v = JSON.parse(args) as Record<string, unknown>;
    const p = v.path ?? v.file_path ?? v.notebook_path;
    return typeof p === "string" ? p : undefined;
  } catch {
    return undefined;
  }
}

type Bucket = { count: number; paths: Set<string> };

/**
 * 从一串步骤里生成摘要片段。按**首次出现顺序**排列，不排序 —— 摘要要读起来
 * 像一句话，顺序本身就是叙事。
 */
export function buildActivitySegments(steps: TimelineStep[]): string[] {
  const buckets = new Map<string, Bucket>();
  const order: string[] = [];

  for (const step of steps) {
    if (step.kind !== "tools") continue;
    for (const ev of step.events) {
      if (ev.type !== "tool_start" || !ev.tool) continue;
      if (!buckets.has(ev.tool)) {
        buckets.set(ev.tool, { count: 0, paths: new Set() });
        order.push(ev.tool);
      }
      const b = buckets.get(ev.tool)!;
      b.count += 1;
      const p = argPath(ev.arguments);
      if (p) b.paths.add(p);
    }
  }
  if (order.length === 0) return [];

  const parts: string[] = [];
  for (const tool of order) {
    const b = buckets.get(tool)!;
    const verb = TOOL_VERBS[tool];
    if (!verb) {
      parts.push(`${tool}${b.count > 1 ? ` (${b.count})` : ""}`);
      continue;
    }
    // 文件类按去重后的路径数算，调用类按次数算。
    const n = verb.unit === "file" && b.paths.size > 0 ? b.paths.size : b.count;
    parts.push(n === 1 ? verb.one : verb.many(n));
  }
  return parts;
}

/** 这一串步骤是否只有一次工具调用、且没有思考 —— 该情况下不生成摘要。 */
export function shouldSummarize(steps: TimelineStep[], reasoningCount: number): boolean {
  if (reasoningCount > 0) return true;
  const toolSteps = steps.filter((s) => s.kind === "tools");
  const total = toolSteps.reduce((acc, s) => acc + s.events.filter((e) => e.type === "tool_start").length, 0);
  return total !== 1;
}

/**
 * 模块级开合记忆。
 *
 * `null` = 跟随运行状态（跑着就展开，跑完就折起）；`true`/`false` = 用户
 * 亲手定了，就一直照办。抄 cc-haha 的 `lib/disclosureMemory`。
 */
const disclosureMemory = new Map<string, boolean>();
const DISCLOSURE_CAP = 200;

export function getDisclosure(key: string): boolean | null {
  return disclosureMemory.get(key) ?? null;
}

export function setDisclosure(key: string, value: boolean): void {
  // 容量上限用插入顺序淘汰最旧的：早的对话早就滚出视野了。
  if (disclosureMemory.size >= DISCLOSURE_CAP) {
    const oldest = disclosureMemory.keys().next().value;
    if (oldest !== undefined) disclosureMemory.delete(oldest);
  }
  disclosureMemory.set(key, value);
}

/**
 * 开合状态。返回 `null` 表示「没有用户决定过」，由调用方回落到
 * `!isLive`（还在跑 → 展开，跑完 → 折起）。
 *
 * 判定用的是**「这一组是不是活跃轮次的尾巴」**，不是「有没有工具正在执行」
 * —— 后者会在每两个步骤之间来回闪：工具刚结束、模型还没开始说下一句的
 * 那一瞬，判定就变了。抄 cc-haha `ActivityGroup.tsx:84-88`。
 *
 * 「是不是活跃尾巴」由调用方判断（它知道 `isStreaming` 与自己是不是最后
 * 一组）；这里只负责记住用户的选择。
 */
export function useDisclosure(key: string): [boolean | null, (v: boolean) => void] {
  const [pinned, setPinned] = useState<boolean | null>(() => getDisclosure(key));
  useEffect(() => {
    setPinned(getDisclosure(key));
  }, [key]);
  const toggle = useCallback(
    (v: boolean) => {
      setDisclosure(key, v);
      setPinned(v);
    },
    [key],
  );
  return [pinned, toggle];
}
