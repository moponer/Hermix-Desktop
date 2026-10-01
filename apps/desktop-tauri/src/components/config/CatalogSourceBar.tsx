// MCP 目录数据源选择条（批次 M2）。
//
// 四源：official / builtin-offline / self-hosted / modelscope（实验性，
// show_experimental 打开才显示）。切换只影响「目录从哪拉」；已配置的
// server 不受影响。env 覆盖生效时整条锁定（HERMIX_MCP_REGISTRY_URL）。
import { useEffect, useState } from "react";
import { ChevronDown, Loader2, PlugZap, Server, CircleDot } from "lucide-react";
import * as api from "../../api/rest";
import type { CatalogSourcesResponse, CatalogProbeResult, CatalogSourceInfo } from "../../api/types";
import { toastError, toastSuccess } from "../../hooks/useToast";

/** 连通性徽标颜色：probe 结果驱动；未探测为灰。 */
function dotColor(r: CatalogProbeResult | null): string {
  if (!r) return "text-text-faint";
  if (r.ok && r.shapeOk) return "text-success";
  return "text-error";
}

export function CatalogSourceBar({ onChanged }: { onChanged: () => void }) {
  const [sources, setSources] = useState<CatalogSourcesResponse | null>(null);
  const [open, setOpen] = useState(false);
  /** 正在保存的源 id（切源自带一次目录重拉，按钮转圈防双击）。 */
  const [switching, setSwitching] = useState<string | null>(null);
  const [probe, setProbe] = useState<CatalogProbeResult | null>(null);
  const [probing, setProbing] = useState(false);
  const [customUrl, setCustomUrl] = useState("");
  const [showExperimental, setShowExperimental] = useState(false);

  const load = () => {
    api.getMcpCatalogSources().then((res) => {
      if (!res) return;
      setSources(res);
      setShowExperimental(res.showExperimental);
      setCustomUrl(res.customUrl ?? "");
    });
  };
  useEffect(load, []);

  const current = sources?.sources.find((s) => s.id === sources.current);
  const visibleSources = (sources?.sources ?? []).filter(
    (s) => !s.experimental || showExperimental,
  );

  const doSwitch = async (s: CatalogSourceInfo) => {
    if (switching || s.id === sources?.current) return;
    setSwitching(s.id);
    try {
      const res = await api.setMcpCatalogSource(s.id, {
        customUrl: s.id === "self-hosted" ? customUrl.trim() || undefined : undefined,
        showExperimental,
      });
      if (!res?.ok) {
        toastError("切换数据源失败");
        return;
      }
      toastSuccess(`目录数据源已切换为「${s.name}」`);
      setOpen(false);
      setProbe(null);
      onChanged();
      load();
    } finally {
      setSwitching(null);
    }
  };

  const doProbe = async () => {
    setProbing(true);
    try {
      const target = sources?.current === "self-hosted" ? customUrl.trim() || undefined : undefined;
      const r = await api.probeMcpCatalogSource(undefined, target);
      setProbe(r);
      if (r?.ok) toastSuccess(`连接正常（${r.latencyMs}ms）`);
      else toastError(r?.error ?? "探测失败");
    } finally {
      setProbing(false);
    }
  };

  if (!sources) return null;

  return (
    <div className="flex items-center gap-2 flex-wrap text-[length:var(--text-meta)]">
      <Server className="icon-sm text-text-muted" />
      <span className="text-text-muted">数据源</span>

      {sources.envOverride ? (
        <span
          className="h-7 px-2.5 inline-flex items-center rounded-md border border-border-secondary bg-bg-tertiary text-text-muted"
          title="HERMIX_MCP_REGISTRY_URL 环境变量已设置，配置文件的源选择被覆盖"
        >
          环境变量锁定：{sources.current}
        </span>
      ) : (
        <div className="relative">
          <button
            onClick={() => setOpen(!open)}
            className="h-7 px-2.5 inline-flex items-center gap-1.5 rounded-md border border-border-secondary bg-bg-card text-text-secondary hover:text-text-primary hover:bg-bg-hover"
          >
            <CircleDot className={`icon-sm ${dotColor(probe)}`} />
            {current?.name ?? sources.current}
            <ChevronDown className="icon-sm text-text-faint" />
          </button>
          {open && (
            <div className="absolute z-30 mt-1 w-72 rounded-lg border border-border-primary bg-bg-elevated shadow-pop p-1.5">
              {visibleSources.map((s) => (
                <button
                  key={s.id}
                  onClick={() => void doSwitch(s)}
                  disabled={switching !== null}
                  className={`w-full text-left px-2.5 py-2 rounded-md flex items-start gap-2 ${
                    s.id === sources.current ? "bg-bg-hover" : "hover:bg-bg-hover"
                  } disabled:opacity-50`}
                >
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-1.5">
                      <span className="text-[length:var(--text-ui)] text-text-primary font-medium">
                        {s.name}
                      </span>
                      {s.id === sources.current && (
                        <span className="text-[length:var(--text-tiny)] text-accent">当前</span>
                      )}
                      {s.experimental && (
                        <span className="text-[length:var(--text-tiny)] text-warning">实验</span>
                      )}
                    </div>
                    <p className="text-[length:var(--text-tiny)] text-text-muted leading-snug">
                      {s.description}
                    </p>
                    {s.id === "self-hosted" && (
                      <input
                        value={customUrl}
                        onChange={(e) => setCustomUrl(e.target.value)}
                        onClick={(e) => e.stopPropagation()}
                        placeholder="https://你的镜像地址（如 http://127.0.0.1:8080）"
                        className="mt-1.5 w-full h-7 px-2 rounded-md border border-border-secondary bg-bg-card text-[length:var(--text-tiny)] outline-none focus:border-border-strong font-mono"
                      />
                    )}
                  </div>
                  {switching === s.id && <Loader2 className="icon-sm animate-spin mt-1" />}
                </button>
              ))}
              <label className="flex items-center gap-2 px-2.5 py-1.5 text-[length:var(--text-tiny)] text-text-muted cursor-pointer">
                <input
                  type="checkbox"
                  checked={showExperimental}
                  onChange={(e) => {
                    setShowExperimental(e.target.checked);
                    void api.setMcpCatalogSource(sources.current, {
                      showExperimental: e.target.checked,
                    });
                  }}
                />
                显示实验性数据源
              </label>
            </div>
          )}
        </div>
      )}

      <button
        onClick={() => void doProbe()}
        disabled={probing}
        className="h-7 px-2.5 inline-flex items-center gap-1 rounded-md border border-border-secondary text-text-secondary hover:text-text-primary hover:bg-bg-hover disabled:opacity-50"
        title="测试当前源连通性"
      >
        {probing ? <Loader2 className="icon-sm animate-spin" /> : <PlugZap className="icon-sm" />}
        测试连接
      </button>
      {probe?.ok && (
        <span className="text-[length:var(--text-tiny)] text-text-faint">
          {probe.latencyMs}ms
        </span>
      )}
    </div>
  );
}
