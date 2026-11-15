// components/settings/DailyMemoryPanel.tsx
//
// 每日记忆（daily_memory.db）：逐日展示 goals / progress / conclusions。
// 会话结束时会自动把会话总结追加进当天的 conclusions（确定性写入，
// 不依赖 LLM 主动调工具），所以即使模型从不调 daily_memory，这里
// 也不是空壳。

import { useEffect, useState, type ReactNode } from "react";
import { Target, TrendingUp, CheckCircle2 } from "lucide-react";
import * as api from "../../api";
import { IconTile, SkeletonCard } from "../ui";

export function DailyMemoryPanel() {
  const [days, setDays] = useState<api.DailyMemoryDay[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    api
      .getDailyMemoryList(60)
      .then((list) => {
        if (!alive) return;
        setDays(list);
        if (list.length > 0) setActive(list[0].date);
      })
      .catch((e) => alive && setError(e instanceof Error ? e.message : String(e)))
      .finally(() => alive && setLoading(false));
    return () => { alive = false; };
  }, []);

  if (loading) {
    return (
      <div className="py-10 flex flex-col items-center gap-3">
        <SkeletonCard lines={3} />
      </div>
    );
  }
  if (error) {
    return (
      <p className="rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] px-4 py-3 text-[length:var(--text-meta)] text-[var(--color-text-secondary)]">
        每日记忆不可用（{error}），请稍后重试。
      </p>
    );
  }
  if (days.length === 0) {
    return (
      <p className="py-10 text-center text-[length:var(--text-meta)] text-[var(--color-text-muted)]">
        还没有每日记忆 — 第一次对话结束后会自动出现当天条目。
      </p>
    );
  }
  const current = days.find((d) => d.date === active) ?? days[0];

  return (
    <div className="grid grid-cols-1 gap-3 md:grid-cols-[180px_1fr]">
      <div className="max-h-[420px] space-y-0.5 overflow-y-auto rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] p-1.5">
        {days.map((d) => (
          <button
            key={d.date}
            onClick={() => setActive(d.date)}
            className={`flex w-full items-center justify-between rounded-md px-2.5 py-1.5 text-left text-[length:var(--text-meta)] ${
              current.date === d.date
                ? "bg-bg-selected text-[var(--color-accent)]"
                : "text-[var(--color-text-primary)] hover:bg-[var(--color-bg-hover)]"
            }`}
          >
            <span className="font-medium">{d.date}</span>
            {d.conclusions.trim() && <span className="size-1.5 rounded-full bg-[var(--color-accent)]" />}
          </button>
        ))}
      </div>
      <div className="space-y-3 rounded-lg bg-[var(--color-bg-card)] shadow-[var(--shadow-paper)] p-3">
        <Section
          title="目标"
          body={current.goals}
          icon={<IconTile icon={<Target className="icon-md" />} tone="amber" />}
        />
        <Section
          title="进展"
          body={current.progress}
          icon={<IconTile icon={<TrendingUp className="icon-md" />} tone="cyan" />}
        />
        <Section
          title="结论"
          body={current.conclusions}
          icon={<IconTile icon={<CheckCircle2 className="icon-md" />} tone="green" />}
        />
      </div>
    </div>
  );
}

function Section({ title, body, icon }: { title: string; body: string; icon?: ReactNode }) {
  return (
    <div>
      <h4 className="flex items-center gap-2 text-[length:var(--text-meta)] font-medium text-[var(--color-text-secondary)]">
        {icon}
        {title}
      </h4>
      <pre className="mt-1 whitespace-pre-wrap rounded-md bg-[var(--color-bg-tertiary)] p-3 text-[length:var(--text-meta)] leading-relaxed text-[var(--color-text-primary)]">
        {body.trim() || "（空）"}
      </pre>
    </div>
  );
}
