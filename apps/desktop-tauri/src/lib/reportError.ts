import { invoke } from "../ipc";

/**
 * 上报一条前端错误。
 *
 * 生产构建用 terser `drop_console` 删掉所有 `console.*`，于是「请求失败」与
 * 「列表本来就是空的」在日志里完全同形 —— 用户报"点了没反应"时没有任何证据。
 * 这条 IPC 把错误写进 `<hermix_home>/logs/frontend.log`（带 scope），是排障时
 * 唯一的持久化来源。
 *
 * 只在**降级点**调用：核心路径的错误应当抛给调用方处理，不要在这里吞掉。
 */
export function reportError(scope: string, err: unknown): void {
  const message =
    err instanceof Error
      ? `${err.name}: ${err.message}`
      : typeof err === "string"
        ? err
        : (() => {
            try {
              return JSON.stringify(err);
            } catch {
              return String(err);
            }
          })();

  // 开发期仍打 console（devtools 里可断点、能看栈）；生产期这句被 terser 删掉。
  console.warn(`[${scope}] ${message}`);

  try {
    void invoke("report_frontend_error", { scope, message }).catch(() => {
      // 上报本身失败不再制造新错误。
    });
  } catch {
    // 非 Tauri 环境（单测 / 浏览器预览）：静默跳过。
  }
}
