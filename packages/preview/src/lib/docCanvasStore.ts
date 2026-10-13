//! 文档画布的流式分流器（协议侧）+ 文档状态 store。
//!
//! ## 协议：`<hermix-canvas page="slide16x9|a4">` marker
//!
//! LLM 在单条回复里至多嵌一个 canvas 块（skill 硬规则）。marker 之后的
//! 文本增量属于画布文档，不再进聊天气泡；`</hermix-canvas>` 收口。
//! 未闭合时流结束（done/error）强制收口。
//!
//! ## 不变量（正确性核心）
//!
//! `tail` 里的字符 = **尚未进过气泡、也尚未上屏**的字符。因此：
//! - feed 返回值直接就是要追加进气泡的文本，调用方无脑拼接；
//! - 每个字符最终恰好出现在气泡或画布之一，不丢字、不重复。
//!
//! ## 流畅性铁律（验收标准）
//!
//! - delta **不经 React**：块数据经订阅回调直达消费者（iframe 渲染
//!   脚本），React 只订阅低频元信息（几何/块数/收口状态）。
//! - **块级缓冲**：半截 HTML 停在 tail，只在完整块边界上屏；旧块永不改写。

/** 页面几何。skill 层按指令类型声明，渲染层不猜（2026-10-13 拍板）。 */
export type CanvasPageGeometry = "slide16x9" | "a4";

/** 块边界标签：slide 用 </slide>，a4 文档用 </section>。 */
const BLOCK_CLOSE: Record<CanvasPageGeometry, string> = {
  slide16x9: "</slide>",
  a4: "</section>",
};

/** 开 marker。锚定 ^：只在"紧随其后的就是合法 marker"时开启画布，
 *  避免在畸形 tag 之后的远处误匹配第二个 tag 造成内容错位。 */
const MARKER_OPEN_RE = /^<hermix-canvas\s+page="(slide16x9|a4)"\s*>/;

export const CANVAS_OPEN_TAG = "<hermix-canvas";
export const CANVAS_CLOSE_TAG = "</hermix-canvas>";

/** 画布元信息（低频变化，React 可安全订阅）。 */
export interface CanvasMeta {
  /** null = 无活动画布。 */
  geometry: CanvasPageGeometry | null;
  /** 已上屏的完整块数（面板角标用，不驱动内容渲染）。 */
  blockCount: number;
  /** 流是否已收口（false = 仍在流式增长）。 */
  closed: boolean;
}

type MetaListener = (meta: CanvasMeta) => void;
type BlockListener = (html: string) => void;

/**
 * 文档画布 store。一次会话至多一个活动画布；新一轮带 marker 的回复
 * 开启下一份（旧份被覆盖——产物由 write_file 落盘，聊天画布是易逝
 * 视图，不搞多画布管理）。
 */
export class DocCanvasStore {
  private meta: CanvasMeta = { geometry: null, blockCount: 0, closed: true };
  private metaListeners = new Set<MetaListener>();
  private blockListeners = new Set<BlockListener>();
  /** 未决缓冲：疑似 marker 前缀（未开启时）或半截块（开启时）。 */
  private tail = "";
  private open = false;
  /** 已上屏块的完整历史。iframe 晚于流加载时的重放数据源；
   *  新画布开启 / reset 时清空。 */
  private blocks: string[] = [];
  /** 元信息合并发射：块驱动的高频变化合并到微任务，React 不逐块渲染。 */
  private metaDirty = false;

  // ── 订阅 ────────────────────────────────────────────────────────
  onMeta(fn: MetaListener): () => void {
    this.metaListeners.add(fn);
    return () => this.metaListeners.delete(fn);
  }
  /** 每个完整块上屏时回调一次（postMessage 桥的发送端）。 */
  onBlock(fn: BlockListener): () => void {
    this.blockListeners.add(fn);
    return () => this.blockListeners.delete(fn);
  }
  getMeta(): CanvasMeta {
    return this.meta;
  }
  /** 已上屏块的历史快照（iframe onload 后重放用）。 */
  getBlocks(): readonly string[] {
    return this.blocks;
  }

  /**
   * 喂入一条文本增量，返回本次应追加进聊天气泡的文本（可能为空串）。
   * marker 及其后的内容已被画布吸收，不出现在返回值里。
   */
  feed(raw: string): string {
    if (this.open) {
      this.feedIntoBuffer(raw);
      return "";
    }
    // 未开启：疑似前缀与本次增量拼回来一起判定（tail 字符尚未消费过，
    // 必须参与本次判定，否则跨 chunk 的 marker 会丢字）。
    const combined = this.tail + raw;
    this.tail = "";
    const idx = combined.indexOf(CANVAS_OPEN_TAG);
    if (idx === -1) {
      // 无完整 tag 起点：尾部疑似前缀留 tail，其余全部进气泡
      //（含上一轮扣留的 prevTail 段——它们从未进过气泡）。
      const tailLen = this.storePartialSuffix(combined);
      return combined.slice(0, combined.length - tailLen);
    }
    const before = combined.slice(0, idx);
    const rest = combined.slice(idx);
    const m = rest.match(MARKER_OPEN_RE);
    if (!m) {
      // "<hermix-canvas" 到了但属性未到齐：从起点起整段留待下一增量。
      this.tail = rest;
      return before;
    }
    this.openCanvas(m[1] as CanvasPageGeometry);
    this.feedIntoBuffer(rest.slice(m[0].length));
    return before;
  }

  /**
   * 流结束：强制收口。开启状态下剩余 tail 作为最后一个块上屏；
   * 未开启但 tail 有疑似前缀残留（流断在 `<hermix-` 这类字面量上），
   * 原样返回给调用方补进气泡——store 不静默吞正文。
   */
  close(): string {
    if (!this.open) {
      const leftover = this.tail;
      this.tail = "";
      return leftover;
    }
    if (this.tail) {
      this.emitBlock(this.tail);
      this.tail = "";
    }
    this.open = false;
    this.meta = { ...this.meta, closed: true };
    this.emitMeta();
    return "";
  }

  /** 面板关闭 / 新会话：彻底重置。 */
  reset(): void {
    this.tail = "";
    this.open = false;
    this.blocks = [];
    this.meta = { geometry: null, blockCount: 0, closed: true };
    this.emitMeta();
  }

  // ── 内部 ────────────────────────────────────────────────────────

  /**
   * 检查 combined 尾部是否是 CANVAS_OPEN_TAG 的真前缀（下一增量可能
   * 补全成 marker）。是则存入 tail 并返回其长度，否则清空返回 0。
   */
  private storePartialSuffix(combined: string): number {
    const maxLen = Math.min(combined.length, CANVAS_OPEN_TAG.length - 1);
    for (let len = maxLen; len > 0; len--) {
      const suffix = combined.slice(combined.length - len);
      if (CANVAS_OPEN_TAG.startsWith(suffix)) {
        this.tail = suffix;
        return len;
      }
    }
    this.tail = "";
    return 0;
  }

  private openCanvas(geometry: CanvasPageGeometry): void {
    this.open = true;
    this.blocks = [];
    this.meta = { geometry, blockCount: 0, closed: false };
    this.emitMeta();
  }

  private feedIntoBuffer(piece: string): void {
    if (!piece) return;
    // close tag 可能横跨增量边界：半截（如 "</hermix-"）已在 tail 里。
    // 在拼接后的完整缓冲里定位收口点，从那里截断，避免半截 close tag
    // 字符残留在块缓冲、最终被 close() 误当成内容上屏。
    const combined = this.tail + piece;
    const closeIdx = combined.indexOf(CANVAS_CLOSE_TAG);
    if (closeIdx !== -1) {
      // 收口前的内容照常走块缓冲；close() 把最后半截块补上屏。
      // 收口后的尾注（skill 禁止 canvas 后再写正文）在此丢弃。
      this.tail = combined.slice(0, closeIdx);
      this.flushCompleteBlocks();
      this.close();
      return;
    }
    this.tail = combined;
    this.flushCompleteBlocks();
  }

  private flushCompleteBlocks(): void {
    const boundary = BLOCK_CLOSE[this.meta.geometry ?? "a4"];
    let idx: number;
    while ((idx = this.tail.indexOf(boundary)) !== -1) {
      const end = idx + boundary.length;
      this.emitBlock(this.tail.slice(0, end));
      this.tail = this.tail.slice(end);
    }
  }

  private emitBlock(html: string): void {
    if (!html.trim()) return;
    this.blocks.push(html);
    this.meta = { ...this.meta, blockCount: this.meta.blockCount + 1 };
    for (const fn of this.blockListeners) fn(html);
    this.scheduleMetaEmit();
  }

  /** 块驱动的元信息变化合并到微任务：一帧多个块只触发一次监听。 */
  private scheduleMetaEmit(): void {
    if (this.metaDirty) return;
    this.metaDirty = true;
    queueMicrotask(() => {
      this.metaDirty = false;
      this.emitMeta();
    });
  }

  private emitMeta(): void {
    for (const fn of this.metaListeners) fn(this.meta);
  }
}

/** 会话级单例：画布是全局易逝视图，与具体组件生命周期解耦。 */
export const docCanvasStore = new DocCanvasStore();
