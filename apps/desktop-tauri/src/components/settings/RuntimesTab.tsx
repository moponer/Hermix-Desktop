// components/settings/RuntimesTab.tsx
//
// 内置运行时开关：Node.js / Python / Git 三类。
//
// 设计边界（写死在 UI 上，别含糊）：
// · "内置" = 随 exe 分发在 <安装目录>/<runtime>/ 下的便携运行时，不依赖
//   用户机器上装没装。探测顺序 内置位 → PATH → 手填覆盖，内置位优先。
// · 关掉某项 ≠ 卸载，只是**不把它放进可用运行时**，工具调用会退回系统
//   PATH 上的同名命令；系统也没有就明确报"未安装"，不假装能用。
// · 体积是真金白银：便携版的核心卖点是零安装，运行时体积直接决定分发包
//   大小。默认只内置 Python（~10MB 压缩包），Node / Git 走按需下载 ——
//   见 docs/RUNTIME_BUNDLING.md 的选型与实测体积。

import { useCallback, useEffect, useState } from "react";
import { Boxes, FileCode2, GitBranch, RefreshCw } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";

type RuntimeKind = "node" | "python" | "git";

interface ProbeEntry {
  source: string;
  path: string;
  exists: boolean;
  version?: string | null;
}

interface ProbeResult {
  entries: ProbeEntry[];
  selected: string | null;
}

const RUNTIMES: {
  kind: RuntimeKind;
  name: string;
  icon: typeof FileCode2;
  /** 内置这一项要付的体积代价（压缩包 / 解压后），UI 如实标出来。 */
  sizeNote: string;
  hint: string;
}[] = [
  {
    kind: "node",
    name: "Node.js",
    icon: FileCode2,
    sizeNote: "按需下载 ~45MB（默认不内置）",
    hint: "跑 JS/TS 脚本与前端构建。体积最大，默认用系统已装的。",
  },
  {
    kind: "python",
    name: "Python",
    icon: FileCode2,
    sizeNote: "内置约 10MB（解压后 ~20MB）",
    hint: "execute_code(python) 与 PDF 兜底转换都靠它，体积最小、收益最高。",
  },
  {
    kind: "git",
    name: "Git",
    icon: GitBranch,
    sizeNote: "按需下载 ~37MB（MinGit 便携包）",
    hint: "版本操作。日常 Git 需求优先走内置的 Rust 实现，不必拉这个包。",
  },
];

export function RuntimesTab() {
  const [probes, setProbes] = useState<Partial<Record<RuntimeKind, ProbeResult>>>({});
  const [loading, setLoading] = useState<Partial<Record<RuntimeKind, boolean>>>({});
  const [error, setError] = useState<string | null>(null);

  const probe = useCallback(async (kind: RuntimeKind) => {
    setLoading((p) => ({ ...p, [kind]: true }));
    try {
      const res = await invoke<ProbeResult>("runtime_detect", { kind });
      setProbes((p) => ({ ...p, [kind]: res }));
      setError(null);
    } catch (e) {
      setError(`${kind} 探测失败：${e instanceof Error ? e.message : String(e)}`);
    } finally {
      setLoading((p) => ({ ...p, [kind]: false }));
    }
  }, []);

  useEffect(() => {
    void Promise.all(RUNTIMES.map((r) => probe(r.kind)));
  }, [probe]);

  return (
    <div className="p-6 space-y-6 max-w-2xl">
      <section>
        <div className="flex items-center gap-2">
          <Boxes className="icon-md" />
          <h3 className="text-[13px] font-semibold text-text-primary">内置运行时</h3>
        </div>
        <p className="text-xs text-text-muted mt-1 leading-relaxed">
          便携版不该依赖用户机器上装了什么。内置位优先于系统 PATH；关掉某项只是不放进可用
          运行时，不会卸载文件。
        </p>
      </section>

      {error && (
        <div className="px-3 py-2 rounded-lg bg-red-500/10 border border-red-500/30 text-xs text-red-400">
          {error}
        </div>
      )}

      <div className="space-y-3">
        {RUNTIMES.map(({ kind, name, icon: Icon, sizeNote, hint }) => {
          const res = probes[kind];
          const hit = res?.entries.find((e) => e.exists);
          const builtin = res?.entries.find((e) => e.source === "builtin" && e.exists);
          return (
            <div
              key={kind}
              className="rounded-xl border border-border-primary bg-bg-card p-4"
            >
              <div className="flex items-start justify-between gap-3">
                <div className="flex items-start gap-2.5 min-w-0">
                  <Icon className="icon-md mt-0.5 shrink-0" />
                  <div className="min-w-0">
                    <div className="text-sm font-medium text-text-primary">{name}</div>
                    <p className="text-xs text-text-muted mt-0.5">{hint}</p>
                  </div>
                </div>
                <button
                  onClick={() => void probe(kind)}
                  disabled={!!loading[kind]}
                  className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg border border-border-secondary text-xs text-text-secondary hover:text-text-primary disabled:opacity-40 shrink-0"
                >
                  <RefreshCw className={`icon-sm ${loading[kind] ? "animate-spin" : ""}`} />
                  {loading[kind] ? "检测中" : "重新检测"}
                </button>
              </div>

              <div className="mt-3 flex items-center gap-2 text-xs">
                <span
                  className={`w-2 h-2 rounded-full shrink-0 ${
                    hit ? "bg-green-400" : res === undefined ? "bg-yellow-400" : "bg-red-400"
                  }`}
                />
                <span className="text-text-secondary truncate">
                  {hit
                    ? `${builtin ? "内置" : "系统"} · ${hit.version ?? "版本未知"}`
                    : res === undefined
                      ? "尚未检测"
                      : "未找到可用运行时"}
                </span>
                <span className="text-text-faint ml-auto shrink-0">{sizeNote}</span>
              </div>

              {hit && (
                <div className="mt-2 truncate font-mono text-[11px] text-text-muted" title={hit.path}>
                  {hit.path}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}
