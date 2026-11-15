// 工作台入口。
//
// 主界面是**工作台本身**（画布 + 它自己绑定的对话侧栏），不是列表 ——
// 用户进这个页面是来用工作台的，列个清单让他再点一次属于多一道手续。
// 列表降级为：多个工作台时出现在上方 tab 条，新建走 tab 条末尾的入口。
//
// 「每个工作台自己的侧栏」靠 session_id 实现：打开某个工作台先确保它有
// 绑定会话，再把宿主 activeSessionId 切过去，右侧 chatNode 随之换成该
// 会话的消息流。切换即隔离，不需要宿主另起一套会话栈。
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";
import { createSession, getSessions, updateSessionMetadata } from "../api/rest";
import type { Session } from "../types";
import {
  workbenchBindSession,
  workbenchCreate,
  workbenchDelete,
  workbenchList,
  workbenchSetPrimary,
  type WorkbenchManifest,
} from "../workbench/api";
import { WorkbenchShell } from "./WorkbenchShell";
import { WorkbenchEmpty } from "./WorkbenchEmpty";
import { markContextMenuHandled } from "./ui";

interface WorkbenchViewProps {
  chatNode: ReactNode;
  onSelectSession: (id: string) => void;
  /** 把本工作台的会话登记给宿主，让对话栏渲染得到。 */
  onRegisterSessions: (list: Session[]) => void;
  /** 当前激活会话 id —— 浮窗历史下拉高亮当前对话线。 */
  activeSessionId: string | null;
}

/**
 * 打开哪个：显式主台优先，没有标记就取最近更新的那个。
 *
 * 自己按 `updated_at` 取最大，不假设数组顺序 —— 后端当前恰好按
 * updated_at 倒序返回，但那是它的实现细节，前端据此取 `list[0]`
 * 属于隐式耦合；后端哪天改了排序（比如按 name），主台就会静默变成
 * 别的。显式比较代价只是一次 reduce。
 */
const pickPrimary = (list: WorkbenchManifest[]) => {
  const marked = list.find((w) => w.is_primary);
  if (marked) return marked;
  return list.reduce<WorkbenchManifest | null>(
    (best, w) => (best === null || w.updated_at > best.updated_at ? w : best),
    null,
  );
};

export function WorkbenchView({
  chatNode,
  onSelectSession,
  onRegisterSessions,
  activeSessionId,
}: WorkbenchViewProps) {
  const [items, setItems] = useState<WorkbenchManifest[]>([]);
  const [active, setActive] = useState<WorkbenchManifest | null>(null);
  const [ready, setReady] = useState(false);
  const [error, setError] = useState<string | null>(null);

  /** tab 按最近更新排 —— 常用工作台会被推到前面，不依赖后端返回顺序。 */
  const load = useCallback(async () => {
    const list = await workbenchList();
    setItems([...list].sort((a, b) => b.updated_at - a.updated_at));
    return list;
  }, []);

  /**
   * 打开工作台：先确保它有绑定会话，再切过去。
   *
   * 三件事：
   * 1. 孤儿检测 —— `manifest.session_id` 可能指向一个已被清理的会话
   *    （手工删表 / 切换账户 / 数据库迁移）。`workbench_ensure_session`
   *    看到已绑就**不覆盖**，会把孤儿 id 一直返回下去 —— `PATCH` 找不到
   *    行也不会报错，结果是「工作台页面打开了，但归属戳永远打不上，对话
   *    漏进主侧栏」。发现孤儿就重建一次会话并绑回去。
   * 2. 戳必须确认写进去了 —— `PATCH` 对不存在的 session_id 返回
   *    `Ok(0 rows)`，前端拿不到「写没写成功」的反馈。打戳后再拉一次该工作台
   *    的会话列表，目标 session 必须出现在里面，否则报错。
   * 3. 登记给宿主 —— 主对话列表按归属过滤掉了工作台会话，这里不补一条
   *    对话栏就没有可渲染的会话对象。
   */
  const open = useCallback(
    async (manifest: WorkbenchManifest) => {
      try {
        // 1. 孤儿检测 —— 判据优先用「本工作台已打戳的会话」，manifest.session_id
        //    只是 hint。**不能只信 manifest.session_id**：open() 之后只更新了
        //    active、没同步 items，tab 重新点开时 items 里的 session_id 是过期
        //    旧值（可能 null），若以它为闸门就会每次都新建一条 —— 这就是
        //    「重启/点 tab 越堆越多」的真正根。用 scope 列表兜底最稳：
        //    - scoped 里命中 manifest.session_id → 直接用（已归位）
        //    - scoped 非空但 id 对不上 → 取第一个已归属本台的，复用不新建
        //    - manifest.session_id 仍存在但还没打戳 → 复用并补戳
        //    - 都没有 → 真孤儿/首次打开，新建 + 无条件绑定
        const [normal, scoped] = await Promise.all([
          getSessions().catch(() => [] as Session[]),
          getSessions(manifest.id).catch(() => [] as Session[]),
        ]);
        const boundInScoped =
          manifest.session_id !== null
            ? scoped.find((s) => s.id === manifest.session_id)
            : undefined;
        let sessionId: string;
        if (boundInScoped) {
          sessionId = boundInScoped.id;
        } else if (scoped.length > 0 && scoped[0].id) {
          sessionId = scoped[0].id;
        } else if (
          manifest.session_id !== null &&
          normal.some((s) => s.id === manifest.session_id)
        ) {
          sessionId = manifest.session_id;
        } else {
          // 必须走无条件绑定的 workbench_bind_session：ensure_session 见
          // 「manifest 已绑」就原样返回旧 id，孤儿 id 永远换不掉，后续
          // PATCH 打戳 0 行直接报错卡死 —— 重建会话就永远建不回去。
          sessionId = await createSession(manifest.name).then((s) => s.id);
          await workbenchBindSession(manifest.id, sessionId);
        }
        // 2. 盖戳。每次都盖：老工作台的会话可能是 migration 之前建的，
        // 补戳让它们归位；已盖过的重复写入同一个值，无副作用。
        const patchResult = await updateSessionMetadata(sessionId, {
          workbench_id: manifest.id,
        });
        // 2b. 打戳必须真实生效 —— 0 行匹配 = 会话行不存在（PATCH 曾静默
        // 返回 ok，导致孤儿堆积）。这里显式失败，走 catch 落 error 而非
        // 假装成功。
        if (patchResult.rows_affected === 0) {
          throw new Error(
            `failed to bind session ${sessionId} to workbench ${manifest.id} (PATCH matched 0 rows — session row missing)`,
          );
        }
        // 3. 验收 —— 重新拉一次工作台列表，确认目标 session 真的被打了戳。
        const verified = await getSessions(manifest.id);
        if (!verified.some((s) => s.id === sessionId)) {
          throw new Error(
            `failed to bind session ${sessionId} to workbench ${manifest.id} \
             (PATCH succeeded but session not found in workbench list)`,
          );
        }
        // 4. 登记给宿主会话表 —— 主对话列表按归属过滤掉了工作台会话，
        // 这里不补一条，对话栏就没有可渲染的会话对象。
        onRegisterSessions(verified);
        setActive({ ...manifest, session_id: sessionId });
        onSelectSession(sessionId);
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    [onSelectSession, onRegisterSessions],
  );

  /**
   * 本工作台开一条**全新**对话线（2026-09-30）。
   *
   * `open` 的策略是"复用优先"（scoped 里有就接着用）—— 适合打开工作台；
   * 但长会话上下文越滚越重，用户需要一个"清空重来、画布保留"的入口。
   * 流程与 open 的孤儿防线一致：create → 无条件绑定 → 盖戳 → 验收，
   * 任何一步失败都显式抛错走 catch，绝不假成功。
   */
  const newSession = useCallback(
    async (manifest: WorkbenchManifest) => {
      try {
        const created = await createSession(manifest.name).then((r) => r.id);
        await workbenchBindSession(manifest.id, created);
        const patch = await updateSessionMetadata(created, {
          workbench_id: manifest.id,
        });
        if (patch.rows_affected === 0) {
          throw new Error(
            `failed to bind session ${created} to workbench ${manifest.id} (PATCH matched 0 rows — session row missing)`,
          );
        }
        const verified = await getSessions(manifest.id);
        if (!verified.some((x) => x.id === created)) {
          throw new Error(
            `failed to bind session ${created} to workbench ${manifest.id} (PATCH succeeded but session not found in workbench list)`,
          );
        }
        onRegisterSessions(verified);
        setActive({ ...manifest, session_id: created });
        onSelectSession(created);
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    [onSelectSession, onRegisterSessions],
  );

  // 首次挂载落到主台 —— 空态只在「一个工作台都没有」时出现。
  //
  // 依赖刻意留空数组：`open` 会调 `onSelectSession`，那会改宿主的
  // activeSessionId，而 `onSelectSession` 自身依赖 activeSessionId ——
  // 于是依赖每轮都变，effect 再跑一次就无限循环（React: Maximum update
  // depth exceeded）。首次解析只该发生一次，之后全由用户点 tab / 新建驱动。
  //
  // 不加 ref 守卫：StrictMode 下 dev 会挂载→卸载→再挂载，守卫会让第二次
  // 直接 return，而第一次的异步结果已被 cleanup 丢弃，页面就永远停在 boot
  // 占位。改用「已打开就不再打开」判据 —— 它与挂载次数无关，天然幂等。
  const openedRef = useRef(false);
  useEffect(() => {
    if (openedRef.current) return;
    let alive = true;
    void (async () => {
      try {
        const list = await load();
        if (!alive) return;
        const primary = pickPrimary(list);
        if (primary) {
          openedRef.current = true;
          await open(primary);
        }
      } catch (e) {
        if (alive) setError(String(e));
      } finally {
        if (alive) setReady(true);
      }
    })();
    return () => {
      alive = false;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /** 新建：建成即打开，并把它设为主台 —— 刚建的就是用户此刻要的。 */
  const create = useCallback(
    async (name: string) => {
      try {
        const manifest = await workbenchCreate(name);
        await workbenchSetPrimary(manifest.id);
        await load();
        await open({ ...manifest, is_primary: true });
      } catch (e) {
        setError(String(e));
      }
    },
    [load, open],
  );

  // 切主台：只改标记，不打断当前正在看的画布 ——
  // 用户点 tab 是想看那个工作台，至于下次进来落哪由右键菜单决定。
  const markPrimary = useCallback(
    async (manifest: WorkbenchManifest) => {
      try {
        await workbenchSetPrimary(manifest.id);
        await load();
        // active 是个快照，主台标记变了它不会自己更新 —— 不同步的话
        // 当前正看的 tab 上的小圆点会留在旧位置，看起来像设置没生效。
        setActive((prev) =>
          prev ? { ...prev, is_primary: prev.id === manifest.id } : prev,
        );
      } catch (e) {
        setError(String(e));
      }
    },
    [load],
  );

  // 删除工作台（软删，目录进 _trash 可找回）。
  // 三件事：清掉绑定会话的归属戳（sessions.db 归 sidecar，只能走 HTTP
  // PATCH，直接删目录会让会话永远卡在指向已消失工作台的状态）→ 软删
  // 目录 → 若删的是当前打开的台，落回剩余列表的主台（没有就进空态）。
  const remove = useCallback(
    async (manifest: WorkbenchManifest) => {
      try {
        // 绑定会话可能已被用户手工删除 —— PATCH 404 时继续删目录，
        // 不能让一个孤儿戳挡住删除本身。
        if (manifest.session_id) {
          try {
            await updateSessionMetadata(manifest.session_id, { workbench_id: null });
          } catch {
            // 会话已不存在：戳随行一起没了，无需处理。
          }
        }
        await workbenchDelete(manifest.id);
        const list = await load();
        setActive((prev) => {
          if (!prev || prev.id !== manifest.id) return prev;
          const next = pickPrimary(list.filter((w) => w.id !== manifest.id));
          if (next) {
            void open(next);
            return { ...next, session_id: next.session_id };
          }
          return null; // 一个不剩 → 空态
        });
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    [load, open],
  );

  if (!ready) return <div className="wb-boot" />;
  if (!active) return <WorkbenchEmpty items={items} onCreate={create} error={error} />;

  return (
    <div className="wb-page">
      <div className="wb-tabs" role="tablist">
        {items.map((wb) => (
          <TabItem
            key={wb.id}
            manifest={wb}
            active={wb.id === active.id}
            onOpen={() => void open(wb)}
            onMarkPrimary={() => void markPrimary(wb)}
            onDelete={() => void remove(wb)}
          />
        ))}
        <NewTab onCreate={create} />
      </div>
      {error && <p className="wb-error-inline">{error}</p>}
      <WorkbenchShell
        manifest={active}
        chatNode={chatNode}
        onNewSession={() => void newSession(active)}
        onSelectSession={onSelectSession}
        activeSessionId={activeSessionId}
      />
    </div>
  );
}

/**
 * 单个 tab：左键打开；右键弹菜单（设为主工作台 / 卸载）。
 *
 * 删除用两段式确认：菜单里点「卸载工作台」先变成「确认卸载？」，
 * 再点一次才真删 —— 误触右键多点一下不至于丢掉一台工作台。
 * 点菜单外任意处收起（document 监听在菜单开着时才挂）。
 */
function TabItem({
  manifest,
  active,
  onOpen,
  onMarkPrimary,
  onDelete,
}: {
  manifest: WorkbenchManifest;
  active: boolean;
  onOpen: () => void;
  onMarkPrimary: () => void;
  onDelete: () => void;
}) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [menuPos, setMenuPos] = useState<{ top: number; left: number }>({
    top: 0,
    left: 0,
  });
  const rootRef = useRef<HTMLDivElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!menuOpen) return;
    const close = (e: MouseEvent) => {
      // 菜单经 portal 挂在 body 上，不在 rootRef 子树里 —— 内点判定必须
      // 把菜单容器也算上，否则点菜单项自身先触发「外点关闭」，菜单被卸，
      // click 永远到不了按钮（卸载/设主台看起来「没反应」）。
      const t = e.target as Node;
      if (!rootRef.current?.contains(t) && !menuRef.current?.contains(t)) {
        setMenuOpen(false);
        setConfirming(false);
      }
    };
    // tab 条有 overflow-x:auto，菜单若留在内部会被纵向裁掉（CSS 规则：
    // 一个轴设 auto 会把另一个轴的 visible 算成 auto）。挂到 body + fixed
    // 定位彻底脱离裁剪；任何滚动都意味着位置失效，直接收起。
    const onScroll = () => {
      setMenuOpen(false);
      setConfirming(false);
    };
    document.addEventListener("mousedown", close);
    window.addEventListener("scroll", onScroll, true);
    window.addEventListener("resize", onScroll);
    return () => {
      document.removeEventListener("mousedown", close);
      window.removeEventListener("scroll", onScroll, true);
      window.removeEventListener("resize", onScroll);
    };
  }, [menuOpen]);

  const openMenu = () => {
    const rect = rootRef.current?.getBoundingClientRect();
    if (rect) setMenuPos({ top: rect.bottom + 4, left: rect.left + rect.width / 2 });
    setConfirming(false);
    setMenuOpen(true);
  };

  const closeMenu = () => {
    setMenuOpen(false);
    setConfirming(false);
  };

  return (
    <div className="wb-tab-item" ref={rootRef}>
      <button
        type="button"
        role="tab"
        aria-selected={active}
        className={`wb-tab${active ? " wb-tab-active" : ""}`}
        onClick={onOpen}
        onContextMenu={(e) => {
          e.preventDefault();
          // tab 自带菜单（重命名 / 打开目录 / 删除），全局菜单让位。
          markContextMenuHandled(e);
          openMenu();
        }}
        title={manifest.name}
      >
        {manifest.is_primary && <span className="wb-tab-dot" aria-label="主工作台" />}
        {manifest.name}
      </button>
      {menuOpen &&
        createPortal(
          <div
            className="wb-tab-menu"
            role="menu"
            ref={menuRef}
            // 水平居中于 tab：left 锚在 tab 中点，translateX(-50%) 回拉半宽。
            style={{
              position: "fixed",
              top: menuPos.top,
              left: menuPos.left,
              transform: "translateX(-50%)",
            }}
          >
            <button
              type="button"
              role="menuitem"
              className="wb-tab-menu-item"
              onClick={() => {
                onMarkPrimary();
                closeMenu();
              }}
            >
              设为主工作台
            </button>
            <button
              type="button"
              role="menuitem"
              className={`wb-tab-menu-item wb-tab-menu-danger${confirming ? " wb-tab-menu-confirm" : ""}`}
              onClick={() => {
                if (!confirming) {
                  setConfirming(true);
                  return;
                }
                onDelete();
                closeMenu();
              }}
              onBlur={() => setConfirming(false)}
              title="代码与数据移入回收站（_trash），可手工找回"
            >
              {confirming ? "确认卸载？" : "卸载工作台"}
            </button>
          </div>,
          document.body,
        )}
    </div>
  );
}

/** tab 条末尾的新建入口：展开成输入框，回车即建。 */
function NewTab({ onCreate }: { onCreate: (name: string) => Promise<void> }) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState("");

  const submit = () => {
    const trimmed = name.trim();
    if (!trimmed) return;
    setName("");
    setOpen(false);
    void onCreate(trimmed);
  };

  if (!open) {
    return (
      <button
        type="button"
        className="wb-tab wb-tab-new"
        onClick={() => setOpen(true)}
        title="新建工作台"
      >
        ＋
      </button>
    );
  }

  return (
    <input
      className="wb-tab-input"
      autoFocus
      value={name}
      placeholder="工作台名称"
      onChange={(e) => setName(e.target.value)}
      onKeyDown={(e) => {
        if (e.key === "Enter") submit();
        if (e.key === "Escape") {
          setName("");
          setOpen(false);
        }
      }}
      onBlur={() => {
        if (!name.trim()) setOpen(false);
      }}
    />
  );
}
