/**
 * 任务面板 —— 阿黎待拉取的需求队列（2026-11-06 重构）。
 *
 * 五条规矩：
 * ① **顺序就是优先级**：`↑/↓` 直接交换相邻项，不发明"高/中/低"标签
 *    （标签一旦存在，就一定与用户真实的先后意图对不上）。
 * ② **不归类**：面板不猜这条需求属于什么类型 —— 是不是与当前任务同一件
 *    事，由阿黎在她那一轮里判断（她的 `【归档】` 行写在状态页）。
 * ③ **被拉取的不再显示**：面板只回答"还没开工的有哪些"，只留一行本批读数。
 * ④ **退回的不自动重试**：她判"缺目标"的需求回到「待你补充」，必须你补充
 *    或取消；否则 4s 一次的拉取会把它反复拉走、反复退回。
 * ⑤ **状态只说真事**：下发出错时把**原始原因**写在这里；失败后是否自动
 *    回队由"这一轮有没有真的动过手"决定（动过就不回，避免重复执行）。
 */
import { X } from "lucide-react";

import { RETURN_LOCK, type TwinBatch, type TwinTask } from "../../lib/twinQueue";
import { pad2 } from "../../lib/readout";

export interface TwinTaskQueueProps {
  queue: TwinTask[];
  batch: TwinBatch | null;
  /** 她这一轮是否在跑 —— 决定表头那句话（新需求本轮结束后接入）。 */
  streaming: boolean;
  onMove: (id: string, dir: -1 | 1) => void;
  onRemove: (id: string) => void;
  /** 把待补充的需求取回草稿（改完重新下达）。 */
  onRevise: (id: string) => void;
  /** 失败批次里还有没放回面板的需求时，给一个明确的重投入口。 */
  onRequeue?: () => void;
}

const BATCH_STATUS: Record<TwinBatch["status"], string> = {
  running: "执行中",
  done: "已完成",
  failed: "已失败",
};

export function TwinTaskQueue({
  queue,
  batch,
  streaming,
  onMove,
  onRemove,
  onRevise,
  onRequeue,
}: TwinTaskQueueProps) {
  const open = queue.filter((t) => !t.blocked);
  const blocked = queue.filter((t) => t.blocked);

  return (
    <div className="twin-queue">
      <div className="twin-queue__head">
        <span className="twin-queue__count">
          待拉取 <b key={open.length}>{pad2(open.length)}</b> 条
        </span>
        <span className="twin-queue__hint">
          {streaming
            ? "她在执行本批；这里新加的需求本轮结束后自动接入"
            : open.length > 0
              ? "她按面板顺序拉取，再自己判断并入还是新开"
              : ""}
        </span>
      </div>

      {open.length === 0 ? (
        <p className="twin-muted twin-queue__empty">
          面板是空的。在下面写一条需求、回车即入队；她拉取后这条会离开面板。
        </p>
      ) : (
        <div className="twin-qlist" aria-label="待拉取的需求（按优先级排序）">
          {open.map((t, i) => (
            <div key={t.id} className="twin-qitem">
              <span className="twin-qitem__no">{pad2(i + 1)}</span>
              <span className="twin-qitem__text" title={t.text}>
                {t.text}
              </span>
              <span className="twin-qitem__acts">
                <button
                  type="button"
                  className="twin-qitem__btn"
                  aria-label={`提升优先级：${t.text}`}
                  title="提升优先级"
                  disabled={i === 0}
                  onClick={() => onMove(t.id, -1)}
                >
                  ↑
                </button>
                <button
                  type="button"
                  className="twin-qitem__btn"
                  aria-label={`降低优先级：${t.text}`}
                  title="降低优先级"
                  disabled={i === open.length - 1}
                  onClick={() => onMove(t.id, 1)}
                >
                  ↓
                </button>
                <button
                  type="button"
                  className="twin-qitem__btn"
                  aria-label={`删除需求：${t.text}`}
                  title="删除这条需求"
                  onClick={() => onRemove(t.id)}
                >
                  <X aria-hidden />
                </button>
              </span>
            </div>
          ))}
        </div>
      )}

      {/* 退回区：她说"缺目标"就没动手，需求回到这里等你补充。
          这一区不参与自动拉取 —— 否则会变成拉取/退回的死循环。 */}
      {blocked.length > 0 ? (
        <div className="twin-qblocked">
          <div className="twin-batch__head">
            <span>待你补充</span>
            <span className="twin-batch__meta">{blocked.length} 条</span>
          </div>
          {blocked.map((t) => (
            <div key={t.id} className="twin-qitem twin-qitem--blocked">
              <div className="twin-qitem__body">
                <span className="twin-qitem__text" title={t.text}>
                  {t.text}
                </span>
                <span className="twin-qitem__reason">
                  缺：{t.blockReason ?? "目标不明确"}
                  {(t.returns ?? 0) >= RETURN_LOCK
                    ? `（已退回 ${t.returns} 次，补充后再提交）`
                    : ""}
                </span>
              </div>
              <span className="twin-qitem__acts">
                <button
                  type="button"
                  className="chip-flat"
                  aria-label={`补充需求：${t.text}`}
                  onClick={() => onRevise(t.id)}
                >
                  补充
                </button>
                <button
                  type="button"
                  className="chip-flat"
                  aria-label={`取消需求：${t.text}`}
                  onClick={() => onRemove(t.id)}
                >
                  取消
                </button>
              </span>
            </div>
          ))}
        </div>
      ) : null}

      {/* 一行读数够用：这是"待拉取"面板，执行与归档归「状态」页。
          同一批需求在两个地方各列一遍，两处一出偏差就没人知道该信哪个。 */}
      <div className="twin-batch__head">
        <span>本批</span>
        <span className="twin-batch__meta">
          {batch ? `${batch.tasks.length} 条 · ${BATCH_STATUS[batch.status]}` : "还没拉取过"}
        </span>
      </div>
      {batch?.error ? (
        <p className="twin-batch__error" role="alert">
          {batch.error}
        </p>
      ) : null}
      {batch?.status === "failed" && !batch.requeued && onRequeue ? (
        <button type="button" className="chip-flat twin-batch__retry" onClick={onRequeue}>
          把这一批放回面板
        </button>
      ) : null}
    </div>
  );
}
