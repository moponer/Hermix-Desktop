/**
 * Composer 输入注入总线。
 *
 * 为什么不是 prop 链：注入来源分散在消息操作（引用/编辑重发）、语音、润色、
 * 工作流预填、附件右键。逐层把 setInput 透传下去会让 ChatView 的 props 再长
 * 一截，而且每加一个来源都要动三层组件。这里只保留"一个活跃 Composer"的
 * 约束（同屏只有一个输入框在编辑），用模块级单监听者 + 队列：
 *   - 没有订阅者时先排队（例如消息页先于输入框挂载）；
 *   - 订阅时把积压请求按序回放。
 * 与 `imageViewer.ts` 同一套"模块级事件"先例。
 */

export type InjectionMode = "replace" | "append" | "at-caret";

export interface InjectionRequest {
  id: string;
  /** 来源，仅用于调试与将来做撤销分组。 */
  source: "prefill" | "quote" | "edit" | "voice" | "polish" | "enhance" | "file" | "manual";
  text: string;
  mode: InjectionMode;
  /** 注入后是否聚焦并把光标放到插入末尾，默认 true。 */
  focus?: boolean;
}

type Listener = (req: InjectionRequest) => void;

let listener: Listener | null = null;
const pending: InjectionRequest[] = [];

let seq = 0;
function nextId(): string {
  seq += 1;
  return `inj-${Date.now()}-${seq}`;
}

/** 注入一段文本到当前 Composer。 */
export function injectIntoComposer(
  req: Omit<InjectionRequest, "id"> & { id?: string },
): void {
  const full: InjectionRequest = { ...req, id: req.id ?? nextId() };
  if (listener) listener(full);
  else pending.push(full);
}

/** ChatView 挂载时订阅；返回取消订阅。 */
export function subscribeComposerInjection(fn: Listener): () => void {
  listener = fn;
  while (pending.length > 0) {
    const req = pending.shift();
    if (req) fn(req);
  }
  return () => {
    if (listener === fn) listener = null;
  };
}
