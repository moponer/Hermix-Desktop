import { useCallback, useEffect, useState } from "react";
import { Shield, FolderOpen, Check, Save } from "lucide-react";
import * as api from "../../api";
import { openInExplorer } from "../../desktopBridge";
import { toastError } from "../../hooks/useToast";
import { useSafeTimeout } from "../../hooks/useSafeTimeout";

interface SecurityState {
  auto_backup: boolean;
  backup_dir: string;
  backup_max_mb: number;
}

const DEFAULT_SECURITY: SecurityState = {
  auto_backup: true,
  backup_dir: "",
  backup_max_mb: 512,
};

function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const kb = n / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  return `${(mb / 1024).toFixed(2)} GB`;
}

export function SecurityTab() {
  const safeSetTimeout = useSafeTimeout();
  const [sec, setSec] = useState<SecurityState>(DEFAULT_SECURITY);
  const [status, setStatus] = useState<api.BackupStatus | null>(null);
  const [loaded, setLoaded] = useState(false);
  const [saved, setSaved] = useState(false);

  const refreshStatus = useCallback(async () => {
    const s = await api.getBackupStatus();
    setStatus(s);
  }, []);

  useEffect(() => {
    (async () => {
      try {
        const cfg = await api.getConfig();
        const raw = (cfg as Record<string, unknown>).security as
          | Partial<SecurityState>
          | undefined;
        if (raw) {
          setSec({
            auto_backup: raw.auto_backup ?? DEFAULT_SECURITY.auto_backup,
            backup_dir: raw.backup_dir ?? "",
            backup_max_mb: raw.backup_max_mb ?? DEFAULT_SECURITY.backup_max_mb,
          });
        }
      } catch {
        /* 用默认值 */
      }
      setLoaded(true);
      void refreshStatus();
    })();
  }, [refreshStatus]);

  const update = (next: Partial<SecurityState>) => {
    setSec((prev) => ({ ...prev, ...next }));
    setSaved(false);
  };

  const handleSave = async () => {
    try {
      // 全量读改写：后端 PUT /api/config 是整份覆盖，局部字段会清掉
      // providers / model 等其它配置，必须发完整 config。
      const cfg = (await api.getConfig()) as Record<string, unknown>;
      const next = {
        ...cfg,
        security: {
          auto_backup: sec.auto_backup,
          backup_dir: sec.backup_dir || null,
          backup_max_mb: sec.backup_max_mb,
        },
      };
      await api.updateConfig(next as unknown as api.HermesConfig);
      setSaved(true);
      safeSetTimeout(() => setSaved(false), 2000);
      // 保存后让后端刷新策略并刷新统计。
      void refreshStatus();
    } catch (e) {
      console.error("Failed to save security config:", e);
      toastError("保存安全设置失败");
      setSaved(false);
    }
  };

  const openBackupDir = async () => {
    if (!status?.dir) {
      toastError("暂无备份目录");
      return;
    }
    await openInExplorer(status.dir);
  };

  return (
    <div className="space-y-5">
      <div>
        <h2 className="text-base font-medium text-text-primary flex items-center gap-2">
          <Shield className="icon-md" /> 安全中心
        </h2>
        <p className="text-xs text-text-muted mt-1">
          agent 通过写文件 / 补丁覆盖你的文件前，先把旧版本存进备份目录。误改可回滚。
        </p>
      </div>

      {/* 自动备份开关 */}
      <div className="rounded-lg border border-border-primary bg-bg-card p-4 space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <div className="text-sm text-text-primary">启用自动备份</div>
            <div className="text-[12px] text-text-muted mt-0.5">
              覆盖写前复制旧版本（best-effort，不影响正常写入）。
            </div>
          </div>
          <button
            role="switch"
            aria-checked={sec.auto_backup}
            onClick={() => update({ auto_backup: !sec.auto_backup })}
            className={`relative h-6 w-11 rounded-full transition-colors ${
              sec.auto_backup ? "bg-accent" : "bg-bg-tertiary"
            }`}
          >
            <span
              className={`absolute top-0.5 left-0.5 h-5 w-5 rounded-full bg-white transition-transform ${
                sec.auto_backup ? "translate-x-5" : "translate-x-0"
              }`}
            />
          </button>
        </div>

        <div>
          <label className="text-[12px] text-text-secondary">备份目录（留空 = 默认）</label>
          <input
            value={sec.backup_dir}
            onChange={(e) => update({ backup_dir: e.target.value })}
            placeholder="留空使用 <数据目录>/backups"
            className="mt-1 w-full px-3 py-2 rounded-lg border border-border-secondary bg-bg-primary text-sm text-text-primary outline-none focus:border-accent transition-colors"
          />
          <p className="mt-1 text-[11px] text-text-muted">
            默认位于 Hermix 数据目录下的 backups 文件夹。
          </p>
        </div>

        <div>
          <label className="text-[12px] text-text-secondary">备份占用上限（MB）</label>
          <input
            type="number"
            min={1}
            value={sec.backup_max_mb}
            onChange={(e) => update({ backup_max_mb: Math.max(1, Number(e.target.value) || 1) })}
            className="mt-1 w-full px-3 py-2 rounded-lg border border-border-secondary bg-bg-primary text-sm text-text-primary outline-none focus:border-accent transition-colors"
          />
          <p className="mt-1 text-[11px] text-text-muted">超过后按时间删除最旧的备份。</p>
        </div>
      </div>

      {/* 备份状态 */}
      <div className="rounded-lg border border-border-primary bg-bg-card p-4 space-y-3">
        <div className="text-sm font-medium text-text-primary">备份状态</div>
        {!loaded ? (
          <div className="text-[12px] text-text-muted">加载中…</div>
        ) : (
          <div className="space-y-2 text-[12px]">
            <Row label="状态" value={status?.enabled ? "已启用" : "未启用"} />
            <Row label="目录" value={status?.dir ?? "—"} mono />
            <Row label="备份文件数" value={String(status?.file_count ?? 0)} />
            <Row label="占用" value={formatBytes(status?.total_bytes ?? 0)} />
            <Row
              label="最近备份"
              value={status?.last_backup ? new Date(Number(status.last_backup) * 1000).toLocaleString() : "—"}
            />
            <button
              onClick={openBackupDir}
              disabled={!status?.dir}
              className="flex items-center gap-2 px-3 py-1.5 mt-1 rounded-lg border border-border-secondary text-text-secondary text-[12px] hover:text-text-primary hover:bg-bg-hover transition-colors disabled:opacity-40"
            >
              <FolderOpen className="icon-md" /> 打开备份目录
            </button>
          </div>
        )}
      </div>

      <button
        onClick={handleSave}
        className="flex items-center gap-2 px-4 py-2 rounded-lg bg-accent text-accent-on-text text-sm font-medium hover:opacity-90 transition-opacity"
      >
        {saved ? (
          <>
            <Check className="icon-md" /> 已保存
          </>
        ) : (
          <>
            <Save className="icon-md" /> 保存设置
          </>
        )}
      </button>
    </div>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex items-start justify-between gap-3">
      <span className="text-text-muted shrink-0">{label}</span>
      <span className={`text-text-primary text-right break-all ${mono ? "font-mono text-[11px]" : ""}`}>
        {value}
      </span>
    </div>
  );
}
