// 任务台（总管方案 P4）：成员工单看板 + 角色库 + 资源水位。
//
// 数据面（member_api.rs）：
//   - GET /api/members          工单列表（轻量，10s 轮询）
//   - GET /api/members/{id}     工单详情（点开抽屉时拉取）
//   - POST /api/members/{id}/stop  中止
//   - GET /api/agent-roles      角色库
//   - GET /api/resource/patrol  资源巡检快照（停派门 / C 盘水位 / 配额）
//
// 设计纪律：颜色/字号只走令牌类；验收状态用语义色文字（--color-text-*）
// 不做色条；无轮询假进度——运行中的卡片只展示已发生的状态字段。
import { useCallback, useMemo, useState } from "react";
import {
  Bot,
  CheckCircle2,
  CircleAlert,
  Gauge,
  KanbanSquare,
  PauseCircle,
  ScrollText,
} from "lucide-react";
import { fetchJSON } from "../api/rest";
import type { IconTone } from "../lib/categoryTone";
import { EmptyState, IconTile, Modal, PageHeader, SegmentedControl, SkeletonCard } from "./ui";
import { toastError } from "../hooks/useToast";
import { usePolling } from "../hooks/usePolling";

type BoardTab = "tickets" | "team" | "resource";

/** `GET /api/members` 的条目（后端已剥掉报告正文）。 */
interface MemberSummary {
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

interface RoleEntry {
  name: string;
  description: string;
  worker: string;
  toolset: string | null;
  model: string | null;
  version: number;
  file: string;
}

/** `GET /api/member-stats` 的角色评分（P6）。 */
interface RoleStats {
  role: string;
  total: number;
  completed: number;
  completion_rate: number;
  review: { pass: number; warn: number; reject: number };
  avg_turns: number;
  avg_seconds: number;
}

interface CacheEntry {
  label: string;
  path: string;
  bytes: number;
}

interface PatrolSnapshot {
  dispatch_blocked: boolean;
  c_drive_low_free_bytes: number | null;
  over_quota: Array<{ path: string; used_bytes: number; cap_bytes: number }>;
}

const ROLE_TONE: Record<string, IconTone> = {
  reviewer: "violet",
  coder: "blue",
};
const FALLBACK_TONE: IconTone = "slate";

/** 拉取失败的就地面板：说清**哪一路**失败（各路独立后这是必需的），
 *  给手动重试；60s 自动轮询照旧，恢复后自动回到正常渲染。 */
function LoadError({ what, text, onRetry }: { what: string; text: string; onRetry: () => void }) {
  return (
    <div className="rounded-[var(--radius-md)] bg-error-soft p-4 text-[length:var(--text-ui)] text-text-error">
      <p className="flex items-start gap-1.5">
        <CircleAlert className="icon-md mt-0.5 shrink-0" />
        <span>
          {what}暂时加载不出来：{text || "后端没有响应"}。下面可以重试，或等它自动再试。
        </span>
      </p>
      <button
        type="button"
        className="mt-2 rounded-[var(--radius-sm)] px-3 py-1.5 text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-tertiary hover:text-text-primary"
        onClick={onRetry}
      >
        重试
      </button>
    </div>
  );
}

/** 一张工单卡。`compact` = 值守页窄容器的单行形态（宽页签用两行卡片）。 */
function MemberCard({
  m,
  compact = false,
  onOpen,
  onStop,
}: {
  m: MemberSummary;
  compact?: boolean;
  onOpen: (id: string) => Promise<void>;
  onStop: (id: string) => Promise<void>;
}) {
  const verdictClass =
    m.review_verdict === "pass"
      ? "text-text-success"
      : m.review_verdict === "warn"
        ? "text-text-warning"
        : "text-text-error";
  return (
    <div
      className={`${compact ? "flex items-center gap-2 px-3 py-2" : "flex items-start gap-2 p-3"} cursor-pointer rounded-[var(--radius-md)] border border-border-subtle bg-bg-secondary transition-colors hover:border-border-primary`}
      onClick={() => void onOpen(m.member_id)}
      role="button"
      tabIndex={0}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") void onOpen(m.member_id);
      }}
    >
      <IconTile icon={<RoleGlyph role={m.role} />} tone={roleTone(m.role)} size="sm" />
      <div className="min-w-0 flex-1">
        <p className={`${compact ? "truncate" : "line-clamp-2"} text-[length:var(--text-ui)] text-text-primary`}>
          {m.task}
        </p>
        <p className={`${compact ? "mt-0" : "mt-1"} text-[length:var(--text-meta)] text-text-faint`}>
          {m.role} · {STATUS_LABEL[m.status]} · {m.total_turns} 轮 · {fmtTime(m.started_at)}
        </p>
        {m.review_verdict && (
          <p className={`mt-0.5 text-[length:var(--text-meta)] ${verdictClass}`}>
            验收：{m.review_verdict === "pass" ? "通过" : m.review_verdict === "warn" ? "有保留" : "未通过"}
          </p>
        )}
      </div>
      {m.status === "running" && (
        <button
          type="button"
          aria-label={`中止工单 ${m.task.slice(0, 20)}`}
          className="shrink-0 rounded-[var(--radius-sm)] p-1 text-text-muted transition-colors hover:bg-error-soft hover:text-text-error"
          onClick={(e) => {
            e.stopPropagation();
            void onStop(m.member_id);
          }}
        >
          <PauseCircle className="icon-md" />
        </button>
      )}
    </div>
  );
}

function roleTone(role: string): IconTone {
  return ROLE_TONE[role] ?? FALLBACK_TONE;
}

/** 角色身份块的字形：首字母反白（字母形态归 IconTile 的既有语义）。 */
function RoleGlyph({ role }: { role: string }) {
  const letter = (role.trim()[0] ?? "?").toUpperCase();
  return <span className="text-[length:var(--text-ui)] font-semibold">{letter}</span>;
}

/** 看板列定义：状态 → 语义分组。 */
function columnOf(m: MemberSummary): 0 | 1 | 2 | 3 {
  if (m.status === "running") return 0;
  if (m.status === "completed") {
    return m.review_verdict === "pass" || m.review_verdict === "warn" ? 2 : 1;
  }
  return 3; // failed / cancelled / timeout / review reject
}

const COLUMNS = [
  { key: "running", title: "进行中" },
  { key: "awaiting", title: "待验收" },
  { key: "delivered", title: "已交付" },
  { key: "trouble", title: "待处置" },
] as const;

/** 终态的中文标签。 */
const STATUS_LABEL: Record<MemberSummary["status"], string> = {
  running: "运行中",
  completed: "已完成",
  failed: "失败",
  cancelled: "已中止",
  timeout: "超时",
};

function fmtTime(iso: string | null): string {
  if (!iso) return "—";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleTimeString("zh-CN", { hour: "2-digit", minute: "2-digit" });
}

function fmtGiB(bytes: number | null): string {
  if (bytes == null) return "—";
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GiB`;
}

export function MissionBoardView({ embedded = false }: { embedded?: boolean } = {}) {
  const [tab, setTab] = useState<BoardTab>("tickets");
  const [members, setMembers] = useState<MemberSummary[] | null>(null);
  const [roles, setRoles] = useState<RoleEntry[] | null>(null);
  const [roleErrors, setRoleErrors] = useState<Array<{ file: string; error: string }>>([]);
  /** 角色库拉取失败的原因；非空 = 显示错误 + 重试，而不是骨架屏。 */
  const [rolesError, setRolesError] = useState<string | null>(null);
  const [patrol, setPatrol] = useState<PatrolSnapshot | null>(null);
  /** 资源巡检拉取失败的原因；同上。 */
  const [patrolError, setPatrolError] = useState<string | null>(null);
  const [roleStats, setRoleStats] = useState<RoleStats[] | null>(null);
  const [caches, setCaches] = useState<CacheEntry[] | null>(null);
  const [cacheScanning, setCacheScanning] = useState(false);
  const [detail, setDetail] = useState<MemberDetail | null>(null);
  const [detailLoading, setDetailLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refreshMembers = useCallback(async () => {
    try {
      const v = await fetchJSON<{ members: MemberSummary[] }>("/api/members");
      setMembers(v.members);
      setError(null);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  // 三路各自独立（2026-10-25 修复）：此前 `Promise.all` 把三个端点捆在一起，
  // 任何一个失败 → roles/patrol 都停在 `null` → 「团队」「资源」两个页签
  // **永久骨架屏**（渲染条件就是 `=== null`，而 catch 里什么都不设）。
  // 现在各查各的：失败的那一路显示错误与重试（60s 自动重试照旧），
  // 成功的路照常渲染。
  const refreshStatic = useCallback(() => {
    fetchJSON<{ roles: RoleEntry[]; errors: Array<{ file: string; error: string }> }>("/api/agent-roles")
      .then((r) => {
        setRoles(r.roles);
        setRoleErrors(r.errors ?? []);
        setRolesError(null);
      })
      .catch((e) => setRolesError(String(e)));
    fetchJSON<PatrolSnapshot>("/api/resource/patrol")
      .then((p) => {
        setPatrol(p);
        setPatrolError(null);
      })
      .catch((e) => setPatrolError(String(e)));
    fetchJSON<{ stats: RoleStats[] }>("/api/member-stats")
      .then((st) => setRoleStats(st.stats))
      .catch(() => setRoleStats(null));
  }, []);

  // members 快轮询；静态面（巡检/角色/评分）慢轮询——资源页签的水位要能自己
  // 恢复显示，不能只靠手动刷新（复查 P2-5）。两者都走 usePolling（P2-4：
  // 值守页常开，隐藏页不该继续打后端）。
  usePolling(refreshMembers, 10_000);
  usePolling(refreshStatic, 60_000);

  const openDetail = useCallback(async (id: string) => {
    setDetailLoading(true);
    try {
      const v = await fetchJSON<MemberDetail>(`/api/members/${id}`);
      setDetail(v);
    } catch {
      setDetail(null);
    } finally {
      setDetailLoading(false);
    }
  }, []);

  const stopMember = useCallback(
    async (id: string) => {
      try {
        await fetchJSON(`/api/members/${id}/stop`, { method: "POST" });
      } catch (e) {
        toastError(`中止失败：${String(e)}`);
      }
      await refreshMembers();
    },
    [refreshMembers],
  );

  const scanCaches = useCallback(async () => {
    setCacheScanning(true);
    try {
      const v = await fetchJSON<{ caches: CacheEntry[] }>("/api/resource/caches");
      setCaches(v.caches);
    } catch {
      setCaches([]);
    } finally {
      setCacheScanning(false);
    }
  }, []);

  const grouped = useMemo(() => {
    const cols: MemberSummary[][] = [[], [], [], []];
    for (const m of members ?? []) cols[columnOf(m)].push(m);
    return cols;
  }, [members]);

  // 看板内容（工单/团队/资源 + 详情 Modal）：独立页与阿黎值守页「任务」
  // 页签共用（总管方案 D8：任务台是阿黎能力的展示面，不是另一个应用）。
  // 错误不再用顶部横幅：每个页签对自己的数据负责，失败就地显示 + 重试。
  const inner = (
    <>
      {tab === "tickets" && (
            members === null && error === null ? (
              <div className="flex flex-col gap-3 py-8">
                <SkeletonCard lines={2} />
                <SkeletonCard lines={2} />
              </div>
            ) : members === null ? (
              <LoadError what="工单列表" text={error ?? ""} onRetry={() => void refreshMembers()} />
            ) : members.length === 0 ? (
              <EmptyState
                variant="plain"
                title="还没有成员工单"
                description="在对话里让阿黎派活（dispatch_member），工单会出现在这里"
              />
            ) : embedded ? (
              /* 值守页「任务」页签的窄容器：四列看板塞进来每列只剩两指宽、
                 大半列还是「空」占位（2026-10-25 用户实测「排版差得要命」）。
                 嵌入态改**按状态分组的紧凑列表**：只显示非空组，一行一单。 */
              <div className="flex flex-col gap-5">
                {COLUMNS.map((col, i) =>
                  grouped[i].length === 0 ? null : (
                    <section key={col.key} aria-label={col.title}>
                      <h2 className="mb-1.5 text-[length:var(--text-ui)] font-semibold text-text-muted">
                        {col.title}
                        <span className="ml-1.5 text-text-faint">{grouped[i].length}</span>
                      </h2>
                      <div className="flex flex-col gap-1.5">
                        {grouped[i].map((m) => (
                          <MemberCard key={m.member_id} m={m} compact onOpen={openDetail} onStop={stopMember} />
                        ))}
                      </div>
                    </section>
                  ),
                )}
              </div>
            ) : (
              /* 列数按**容器**宽度决定，不按视口。
                 此前用 `lg:grid-cols-4`（视口断点）：值守页中轴只有 ~300px 宽，
                 但窗口是 1280px → `lg:` 命中 → 四列硬塞进 300px → 每列 40px →
                 中文逐字竖排（2026-10-25 用户实测截图）。这个布局里视口宽度与
                 容器宽度不是一回事，断点必须按容器来（@container）。 */
              <div className="@container">
                <div className="grid grid-cols-1 gap-4 @2xl:grid-cols-2 @5xl:grid-cols-4">
                  {COLUMNS.map((col, i) => (
                    <section key={col.key} aria-label={col.title}>
                      <h2 className="mb-2 flex items-center gap-2 text-[length:var(--text-ui)] font-semibold text-text-muted">
                        {col.title}
                        <span className="text-text-faint">{grouped[i].length}</span>
                      </h2>
                      <div className="flex flex-col gap-2">
                        {grouped[i].length === 0 && (
                          <div className="rounded-[var(--radius-md)] border border-border-subtle px-3 py-4 text-center text-[length:var(--text-meta)] text-text-faint">
                            空
                          </div>
                        )}
                        {grouped[i].map((m) => (
                          <MemberCard key={m.member_id} m={m} onOpen={openDetail} onStop={stopMember} />
                        ))}
                      </div>
                    </section>
                  ))}
                </div>
              </div>
            )
          )}

          {tab === "team" && (
            rolesError != null ? (
              <LoadError what="角色库" text={rolesError} onRetry={refreshStatic} />
            ) : roles === null ? (
              <div className="flex flex-col gap-3 py-8">
                <SkeletonCard lines={2} />
                <SkeletonCard lines={2} />
              </div>
            ) : roles.length === 0 ? (
              <EmptyState
                variant="plain"
                title="角色库是空的"
                description="在对话里让阿黎用 agent_role_upsert 增改角色，此处热加载展示"
              />
            ) : (
              <>
                <p className="mb-4 text-[length:var(--text-ui)] text-text-muted">
                  角色库定义在 <code className="text-text-secondary">~/.hermix/agents/*.md</code>；
                  在对话里让阿黎用 agent_role_upsert 增改，此处热加载展示。
                </p>
                {roleErrors.length > 0 && (
                  <div className="mb-4 rounded-[var(--radius-md)] bg-warning-soft p-3 text-[length:var(--text-ui)] text-text-warning">
                    {roleErrors.length} 个角色文件解析失败（已跳过）：
                    {roleErrors.map((e) => (
                      <p key={e.file} className="mt-1 text-[length:var(--text-meta)]">
                        {e.file} — {e.error}
                      </p>
                    ))}
                  </div>
                )}
                <div className="flex flex-col gap-2">
                  {roles.map((r) => (
                    <div
                      key={r.name}
                      className="flex items-start gap-3 rounded-[var(--radius-md)] border border-border-subtle bg-bg-secondary p-3"
                    >
                      <IconTile icon={<RoleGlyph role={r.name} />} tone={roleTone(r.name)} size="md" />
                      <div className="min-w-0 flex-1">
                        <p className="text-[length:var(--text-ui)] font-medium text-text-primary">
                          {r.name}
                          <span className="ml-2 text-[length:var(--text-meta)] font-normal text-text-faint">
                            {r.worker} · v{r.version}
                            {r.toolset ? ` · ${r.toolset}` : ""}
                            {r.model ? ` · ${r.model}` : ""}
                          </span>
                        </p>
                        <p className="mt-0.5 text-[length:var(--text-meta)] text-text-muted">{r.description}</p>
                        {(() => {
                          const st = roleStats?.find((x) => x.role === r.name);
                          if (!st || st.total === 0) return null;
                          return (
                            <p className="mt-1 text-[length:var(--text-meta)] text-text-faint">
                              评分：完成 {st.completed}/{st.total} · 验收 {st.review.pass}过/{st.review.warn}留/{st.review.reject}拒 · 均 {st.avg_turns} 轮
                            </p>
                          );
                        })()}
                      </div>
                    </div>
                  ))}
                </div>
              </>
            )
          )}

          {tab === "resource" && (
            patrolError != null ? (
              <LoadError what="资源巡检" text={patrolError} onRetry={refreshStatic} />
            ) : patrol === null ? (
              <div className="flex flex-col gap-3 py-8">
                <SkeletonCard lines={2} />
              </div>
            ) : (
              <div className="flex flex-col gap-3">
                {patrol.dispatch_blocked ? (
                  <div className="flex items-start gap-2 rounded-[var(--radius-md)] bg-error-soft p-3 text-[length:var(--text-ui)] text-text-error">
                    <CircleAlert className="icon-md mt-0.5 shrink-0" />
                    <span>
                      资源闸门越限，已停派新工单。C 盘剩余 {fmtGiB(patrol.c_drive_low_free_bytes)}；清理空间后自动恢复。
                    </span>
                  </div>
                ) : (
                  <div className="flex items-start gap-2 rounded-[var(--radius-md)] bg-success-soft p-3 text-[length:var(--text-ui)] text-text-success">
                    <CheckCircle2 className="icon-md mt-0.5 shrink-0" />
                    <span>资源闸门正常，工单派发放行中。巡检每 60 秒一轮。</span>
                  </div>
                )}
                <div className="rounded-[var(--radius-md)] border border-border-subtle bg-bg-secondary p-4">
                  <h3 className="flex items-center gap-2 text-[length:var(--text-ui)] font-semibold text-text-primary">
                    <Gauge className="icon-md" /> C 盘水位
                  </h3>
                  <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">
                    {patrol.c_drive_low_free_bytes == null
                      ? "剩余空间高于 10 GiB 水位线"
                      : `剩余 ${fmtGiB(patrol.c_drive_low_free_bytes)}，低于水位线`}
                  </p>
                </div>
                <div className="rounded-[var(--radius-md)] border border-border-subtle bg-bg-secondary p-4">
                  <div className="flex items-center justify-between gap-2">
                    <h3 className="flex items-center gap-2 text-[length:var(--text-ui)] font-semibold text-text-primary">
                      <ScrollText className="icon-md" /> 缓存占用（P0 清单）
                    </h3>
                    <button
                      type="button"
                      disabled={cacheScanning}
                      onClick={() => void scanCaches()}
                      className="flex items-center gap-1.5 rounded-lg px-3 py-1.5 text-[length:var(--text-meta)] text-text-secondary transition-colors hover:bg-bg-tertiary hover:text-text-primary disabled:opacity-40"
                    >
                      <Gauge className="icon-sm" /> {cacheScanning ? "扫描中" : "扫描缓存占用"}
                    </button>
                  </div>
                  {caches === null ? (
                    <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">
                      按需扫描 npm/pnpm/pip/cargo/Temp 的磁盘占用（只报不删，删除由你决策）。
                    </p>
                  ) : (
                    caches.map((c) => (
                      <p key={c.path} className="mt-1 text-[length:var(--text-meta)] text-text-secondary">
                        {c.label} — {fmtGiB(c.bytes)} <span className="text-text-faint">{c.path}</span>
                      </p>
                    ))
                  )}
                </div>
                <div className="rounded-[var(--radius-md)] border border-border-subtle bg-bg-secondary p-4">
                  <h3 className="flex items-center gap-2 text-[length:var(--text-ui)] font-semibold text-text-primary">
                    <ScrollText className="icon-md" /> scratch 配额
                  </h3>
                  {patrol.over_quota.length === 0 ? (
                    <p className="mt-1 text-[length:var(--text-meta)] text-text-muted">没有超配额目录。</p>
                  ) : (
                    patrol.over_quota.map((q) => (
                      <p key={q.path} className="mt-1 text-[length:var(--text-meta)] text-text-error">
                        {q.path} — {fmtGiB(q.used_bytes)} / {fmtGiB(q.cap_bytes)}
                      </p>
                    ))
                  )}
                </div>
              </div>
            )
          )}
      {/* 工单详情抽屉：判据 / 步骤日志 / 最终报告 / 评审证据 */}
      <Modal
        open={detail !== null || detailLoading}
        onClose={() => setDetail(null)}
        title={detail ? `工单 · ${detail.role}` : "工单"}
        width="wide"
      >
        {detailLoading || detail === null ? (
          <SkeletonCard lines={4} />
        ) : (
          <div className="flex flex-col gap-4">
            <p className="text-[length:var(--text-ui)] text-text-primary">{detail.task}</p>
            <p className="text-[length:var(--text-meta)] text-text-faint">
              {STATUS_LABEL[detail.status]} · {detail.total_turns} 轮 · {fmtTime(detail.started_at)} → {fmtTime(detail.ended_at)}
            </p>
            {detail.acceptance_criteria.length > 0 && (
              <div>
                <h3 className="mb-1 text-[length:var(--text-ui)] font-semibold text-text-primary">验收判据</h3>
                <ol className="list-decimal pl-5 text-[length:var(--text-ui)] text-text-secondary">
                  {detail.acceptance_criteria.map((c, i) => (
                    <li key={i}>{c}</li>
                  ))}
                </ol>
              </div>
            )}
            {detail.recent_steps.length > 0 && (
              <div>
                <h3 className="mb-1 text-[length:var(--text-ui)] font-semibold text-text-primary">最近步骤</h3>
                <ul className="flex flex-col gap-0.5 text-[length:var(--text-meta)] text-text-muted">
                  {detail.recent_steps.map((s, i) => (
                    <li key={i}>{s}</li>
                  ))}
                </ul>
              </div>
            )}
            {detail.final_report && (
              <div>
                <h3 className="mb-1 text-[length:var(--text-ui)] font-semibold text-text-primary">最终报告</h3>
                <pre className="max-h-64 overflow-y-auto whitespace-pre-wrap rounded-[var(--radius-md)] bg-bg-tertiary p-3 text-[length:var(--text-meta)] text-text-secondary">
                  {detail.final_report}
                </pre>
              </div>
            )}
            {detail.review && (
              <div>
                <h3 className="mb-1 flex items-center gap-2 text-[length:var(--text-ui)] font-semibold text-text-primary">
                  <Bot className="icon-md" /> 评审结论：{detail.review.verdict}
                </h3>
                <pre className="max-h-40 overflow-y-auto whitespace-pre-wrap rounded-[var(--radius-md)] bg-bg-tertiary p-3 text-[length:var(--text-meta)] text-text-secondary">
                  {detail.review.evidence}
                </pre>
              </div>
            )}
            {detail.status === "running" && (
              <div className="flex justify-end">
                <button
                  type="button"
                  className="flex items-center gap-1.5 rounded-lg px-3 py-2 text-[length:var(--text-ui)] text-text-secondary transition-colors hover:bg-error-soft hover:text-text-error"
                  onClick={() => {
                    void stopMember(detail.member_id).then(() => setDetail(null));
                  }}
                >
                  <PauseCircle className="icon-md" /> 中止工单
                </button>
              </div>
            )}
          </div>
        )}
      </Modal>
    </>
  );

  if (embedded) {
    // 阿黎值守页中轴「任务」页签：窄容器、自带分段条与滚动。
    return (
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex h-10 shrink-0 items-center px-4">
          <SegmentedControl
            label="任务台视图"
            as="tablist"
            size="sm"
            value={tab}
            onChange={setTab}
            options={[
              { value: "tickets", label: `工单${members ? ` ${members.length}` : ""}` },
              { value: "team", label: "团队" },
              { value: "resource", label: "资源" },
            ]}
          />
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto px-4 pb-4">
          <div className="mx-auto max-w-5xl">{inner}</div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex h-full flex-col">
      <PageHeader icon={<KanbanSquare className="icon-lg" />} title="任务台" count={members?.length ?? 0}>
        <SegmentedControl
          label="任务台视图"
          as="tablist"
          size="sm"
          value={tab}
          onChange={setTab}
          options={[
            { value: "tickets", label: `任务${members ? ` ${members.length}` : ""}` },
            { value: "team", label: "团队" },
            { value: "resource", label: "资源" },
          ]}
        />
      </PageHeader>
      <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-6">
        <div className="mx-auto max-w-6xl">{inner}</div>
      </div>
    </div>
  );
}
