import { invoke as tauriInvoke, type InvokeArgs } from "@tauri-apps/api/core";

/**
 * 统一 IPC 入口（P2-2）。
 *
 * 为什么需要这一层：Tauri 的 `invoke` 失败时 **reject 的是 string**（不是
 * `Error`），于是几乎每个调用点都要写一遍
 * `e instanceof Error ? e.message : String(e)`——这类判断散落在 19 个文件、
 * 上百处调用里，漏写一处就会出现 `[object Object]` 或 `undefined` 的错误
 * 文案。这里统一转成 `Error`（message 保持原文），调用点只写 `catch (e)`
 * 即可拿到标准错误对象。
 *
 * 超时**默认关闭**：选目录、保存截图、OCR 这类命令可能合法地跑很久，强加
 * 截止时间会把它们误杀。确实需要上限的调用点显式传 `{ timeoutMs }`。
 *
 * 新增代码请从这里 import，不要再直接引 `@tauri-apps/api/core`。
 */
export async function invoke<T>(
  cmd: string,
  args?: InvokeArgs,
  opts?: { timeoutMs?: number },
): Promise<T> {
  const call = tauriInvoke<T>(cmd, args);
  const timeoutMs = opts?.timeoutMs;

  if (!timeoutMs || timeoutMs <= 0) {
    return call.catch(normalizeIpcError);
  }

  return Promise.race([
    call,
    new Promise<never>((_, reject) =>
      setTimeout(
        () => reject(new Error(`IPC "${cmd}" timed out after ${timeoutMs}ms`)),
        timeoutMs,
      ),
    ),
  ]).catch(normalizeIpcError);
}

/** Tauri 的 reject 值是 string；统一成 `Error` 并保留原文。 */
function normalizeIpcError(e: unknown): never {
  if (e instanceof Error) throw e;
  throw new Error(typeof e === "string" ? e : String(e));
}

export type { InvokeArgs };
