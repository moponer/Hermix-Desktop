/**
 * 阿黎对话流里的「部署 / 汇报」卡。
 *
 * 为什么需要它：阿黎自己不产出文件，她交付的是**工单结果**。此前她的页面
 * 只有六行纯文本 —— 派了谁、做到哪一步、结果是什么全都看不见，要看得切到
 * 「团队看板」里翻。用户在实报里说得很直接：「对话栏显示的应该是阿黎的部署
 * 和任务完成说明」「不存在产物，那么结果该如何让用户查看」。
 *
 * 这里把本批工单直接摆进她的对话流，分两段读：
 *   部署  —— 谁在做（角色）、做什么（工单正文）、判据是什么
 *   汇报  —— 做到第几轮、验收判定、最终报告正文（就地展开，不跳页）
 *
 * 数据纪律：全部来自 `GET /api/members` 与 `GET /api/members/{id}`，
 * **不解析她的回复去猜状态**。她说什么是一回事，后端记的工单状态是另一回事；
 * 卡片只认后者，否则"进度"就是编的。
 *
 * 轮询 8s，与值守页其余面板同频；列表接口已剥掉正文，详情只在展开时拉，
 * 且**运行中展开期间**跟着轮询（否则"跟进过程"停在展开那一刻）。
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { fetchJSON } from "../../api/rest";
import { usePolling } from "../../hooks/usePolling";
import { relativeCompact } from "../../lib/readout";

/** `GET /api/members` 的条目（后端已剥掉报告正文）。 */
export interface MemberSummary {
  member_id: string;
  role: string;
  task: string;
  status: "running" | "completed" | "failed" | "cancelled" | "timeout";
  started_at: string;
  ended_at: string | null;
  total_turns: number;
  review_verdict: "pass" | "warn" | "reject" | null;
}

/** `GET /api/members/{id}` 的详情（结构对齐 runner::get 的 JSON）。 */
interface MemberDetail extends MemberSummary {
  acceptance_criteria: string[];
  recent_steps: string[];
  final_report: string | null;
  review: { verdict: string; evidence: string; reviewer_member_id: string } | null;
}

const POLL_MS = 8000;
/** 对话流里最多铺几张卡 —— 再多就不是"本批汇报"，是一份看板了。 */
const MAX_CARDS = 4;

const STATUS_LABEL: Record<MemberSummary["status"], string> = {
  running: "进行中",
  completed: "已完成",
  failed: "失败",
  cancelled: "已中止",
  timeout: "超时",
};

const VERDICT_LABEL: Record<string, string> = {
  pass: "验收通过",
  warn: "验收有保留",
  reject: "验收未通过",
};

/** 一眼定性：卡片的左侧轨道与验收字用同一套判定。 */
function toneOf(m: MemberSummary): "run" | "pass" | "warn" | "fail" {
  if (m.status === "running") return "run";
  if (m.status === "completed") {
    if (m.review_verdict === "warn") return "warn";
    if (m.review_verdict === "reject") return "fail";
    return "pass";
  }
  return "fail";
}

export interface TwinReportCardsProps {
  /**
   * 本批开工时刻（ms）。`null` = 刚重启、批次已丢，此时退回"最近几单"，
   * 让用户至少能看到她上一段在做什么。
   */
  since: number | null;
  /**
   * 请她对某张已结束的工单走验收 + 汇报。
   *
   * 成员跑完不会自己叫醒她（心跳巡查的边界是"零 LLM"，见 `lib/twinPatrol`），
   * 所以「拿回结果并汇报」需要一个明确入口：用户点一下，这条作为一条需求
   * 进面板，她在下一批里 `review_member` + 汇报。**不自动触发** —— 每次
   * 完成都自动开一轮是花钱买噪音。
   */
  onRequestReport?: (m: MemberSummary) => void;
}

export function TwinReportCards({ since, onRequestReport }: TwinReportCardsProps) {
  const [members, setMembers] = useState<MemberSummary[] | null>(null);
  const [openId, setOpenId] = useState<string | null>(null);
  const [detail, setDetail] = useState<MemberDetail | null>(null);
  /** detail 请求失败时如实说，不把空当"没有报告"。 */
  const [detailError, setDetailError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      const v = await fetchJSON<{ members: MemberSummary[] }>("/api/members");
      setMembers(v.members);
    } catch {
      /* 列表拉不到就保留上一份：卡片消失比显示旧数据更像"活干丢了" */
    }
  }, []);

  usePolling(load, POLL_MS);

  const cards = useMemo(() => {
    const list = members ?? [];
    // `since == null`（App 重启、批次已丢）时只显示**还在跑的**：历史工单
    // 在「团队看板」里翻，别一进页就铺四张旧卡把对话流顶下去。
    const scoped =
      since == null
        ? list.filter((m) => m.status === "running")
        : list.filter((m) => {
            const t = Date.parse(m.started_at);
            return Number.isNaN(t) ? false : t >= since;
          });
    // 运行中的排前面（它们是"现在需要你看一眼的"），其余按开工时间倒序。
    return [...scoped]
      .sort((a, b) => {
        const ra = a.status === "running" ? 1 : 0;
        const rb = b.status === "running" ? 1 : 0;
        if (ra !== rb) return rb - ra;
        return (b.started_at ?? "").localeCompare(a.started_at ?? "");
      })
      .slice(0, MAX_CARDS);
  }, [members, since]);

  const reloadDetail = useCallback(async (id: string) => {
    try {
      const v = await fetchJSON<MemberDetail>(`/api/members/${id}`);
      setDetail(v);
      setDetailError(null);
    } catch (e) {
      setDetailError(String(e));
    }
  }, []);

  const toggle = useCallback(
    (m: MemberSummary) => {
      if (openId === m.member_id) {
        setOpenId(null);
        return;
      }
      setOpenId(m.member_id);
      setDetail(null);
      setDetailError(null);
      void reloadDetail(m.member_id);
    },
    [openId, reloadDetail],
  );

  const openMember = cards.find((c) => c.member_id === openId) ?? null;

  // 运行中的工单：展开期间跟着轮询详情 —— 否则"跟进过程"停在展开那一秒，
  // 用户以为卡住了。完成/失败后自动停（状态由列表轮询更新）。
  useEffect(() => {
    if (!openId || openMember?.status !== "running") return;
    const timer = window.setInterval(() => void reloadDetail(openId), POLL_MS);
    return () => window.clearInterval(timer);
  }, [openId, openMember?.status, reloadDetail]);

  // 换了一批（新需求开工）就把展开态收起来：卡片内容已经换人了。
  useEffect(() => {
    setOpenId(null);
    setDetail(null);
  }, [since]);

  if (cards.length === 0) return null;

  return (
    <section className="tw-report" aria-label="这一批的团队工单">
      <header className="tw-report__head">
        <span className="tw-report__title">团队</span>
        <span className="tw-report__count">{cards.length} 单</span>
      </header>
      {cards.map((m) => {
        const tone = toneOf(m);
        const open = openId === m.member_id;
        return (
          <article key={m.member_id} className="tw-report__card" data-tone={tone}>
            <button
              type="button"
              className="tw-report__row"
              aria-expanded={open}
              onClick={() => toggle(m)}
            >
              {open ? (
                <ChevronDown className="icon-xs tw-report__chev" aria-hidden />
              ) : (
                <ChevronRight className="icon-xs tw-report__chev" aria-hidden />
              )}
              <span className="tw-report__role">{m.role}</span>
              <span className="tw-report__state">{STATUS_LABEL[m.status]}</span>
              {m.review_verdict ? (
                <span className="tw-report__verdict" data-tone={tone}>
                  {VERDICT_LABEL[m.review_verdict] ?? m.review_verdict}
                </span>
              ) : null}
              <span className="tw-report__meta">
                {m.total_turns > 0 ? `${m.total_turns} 轮 · ` : ""}
                <span title={m.started_at}>{relativeCompact(m.started_at)}</span>
              </span>
            </button>
            <p className="tw-report__task" title={m.task}>
              {m.task}
            </p>

            {open ? (
              <div className="tw-report__body">
                {detailError ? (
                  <p className="tw-report__note" role="alert">
                    详情读取失败：{detailError}
                  </p>
                ) : detail == null ? (
                  <p className="tw-report__note">正在读取工单…</p>
                ) : (
                  <>
                    {detail.acceptance_criteria.length > 0 ? (
                      <div className="tw-report__block">
                        <p className="tw-report__label">判据</p>
                        <ol className="tw-report__criteria">
                          {detail.acceptance_criteria.map((c, i) => (
                            <li key={i}>{c}</li>
                          ))}
                        </ol>
                      </div>
                    ) : null}

                    {detail.recent_steps.length > 0 ? (
                      <div className="tw-report__block">
                        <p className="tw-report__label">步骤</p>
                        <ul className="tw-report__steps">
                          {detail.recent_steps.map((s, i) => (
                            <li key={i}>{s}</li>
                          ))}
                        </ul>
                      </div>
                    ) : null}

                    <div className="tw-report__block">
                      <p className="tw-report__label">
                        {detail.final_report ? "交付说明" : "交付说明 · 还没写"}
                      </p>
                      {detail.final_report ? (
                        // 报告是成员的正文交付物：保持原样的纯文本，不做 markdown
                        // 再造（里面常有逐行日志/路径，重排反而失真）。
                        <pre className="tw-report__report">{detail.final_report}</pre>
                      ) : (
                        <p className="tw-report__note">
                          {detail.status === "running"
                            ? "成员还在执行，报告会在结束时写回这里。"
                            : "这单没有留下报告正文。"}
                        </p>
                      )}
                    </div>

                    {detail.review ? (
                      <div className="tw-report__block">
                        <p className="tw-report__label">验收 · {detail.review.verdict}</p>
                        <pre className="tw-report__report tw-report__report--review">
                          {detail.review.evidence}
                        </pre>
                      </div>
                    ) : null}

                    {onRequestReport && m.status !== "running" ? (
                      <button
                        type="button"
                        className="chip-flat tw-report__act"
                        onClick={() => onRequestReport(m)}
                      >
                        让她验收并汇报
                      </button>
                    ) : null}
                  </>
                )}
              </div>
            ) : null}
          </article>
        );
      })}
    </section>
  );
}
