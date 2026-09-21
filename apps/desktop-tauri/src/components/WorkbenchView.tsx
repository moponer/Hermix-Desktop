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
import { createSession } from "../api/rest";
import {
  workbenchCreate,
  workbenchEnsureSession,
  workbenchList,
  workbenchSetPrimary,
  type WorkbenchManifest,
} from "../workbench/api";
import { WorkbenchShell } from "./WorkbenchShell";
import { WorkbenchEmpty } from "./WorkbenchEmpty";

interface WorkbenchViewProps {
  chatNode: ReactNode;
  onSelectSession: (id: string) => void;
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

export function WorkbenchView({ chatNode, onSelectSession }: WorkbenchViewProps) {
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
   * 已绑定的工作台沿用原会话 —— 每次新建会让 skill 常驻漂到新会话，
   * 而画布用的是当前 active 会话，一错位 agent 就没了工作台 skill。
   */
  const open = useCallback(
    async (manifest: WorkbenchManifest) => {
      try {
        const fresh = manifest.session_id ?? (await createSession(manifest.name)).id;
        const sessionId = await workbenchEnsureSession(manifest.id, fresh);
        setActive({ ...manifest, session_id: sessionId });
        onSelectSession(sessionId);
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    },
    [onSelectSession],
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

  if (!ready) return <div className="wb-boot" />;
  if (!active) return <WorkbenchEmpty items={items} onCreate={create} error={error} />;

  return (
    <div className="wb-page">
      <div className="wb-tabs" role="tablist">
        {items.map((wb) => (
          <button
            key={wb.id}
            type="button"
            role="tab"
            aria-selected={wb.id === active.id}
            className={`wb-tab${wb.id === active.id ? " wb-tab-active" : ""}`}
            onClick={() => void open(wb)}
            onContextMenu={(e) => {
              e.preventDefault();
              void markPrimary(wb);
            }}
            title="右键设为主工作台（下次打开工作台页面时默认显示它）"
          >
            {wb.is_primary && <span className="wb-tab-dot" aria-label="主工作台" />}
            {wb.name}
          </button>
        ))}
        <NewTab onCreate={create} />
      </div>
      {error && <p className="wb-error">{error}</p>}
      <WorkbenchShell manifest={active} chatNode={chatNode} />
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
