import { useCallback, useEffect, useState } from "react";
import { FolderOpen, Check, Save } from "lucide-react";
import * as api from "../../api";
import { openInExplorer } from "../../desktopBridge";
import { toastError } from "../../hooks/useToast";
import { useSafeTimeout } from "../../hooks/useSafeTimeout";
import { Button, Card, Input, SkeletonCard, Switch } from "../ui";
import { SAVE, SAVED } from "../strings";

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
      /* 真实原因不能只写 console：用户要知道是后端没起来还是配置被拒，
         以及下一步做什么。 */
      toastError(
        `保存安全设置失败：${e instanceof Error ? e.message : String(e)}。请确认后端服务已启动，然后重试。`,
      );
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
      {/* 自动备份开关 */}
      <Card className="space-y-4">
        <div className="flex items-center justify-between">
          <div>
            <div className="text-sm text-text-primary">启用自动备份</div>
            <div className="text-[length:var(--text-meta)] text-text-muted mt-0.5">
              覆盖写前复制旧版本（best-effort，不影响正常写入）。
            </div>
          </div>
          <Switch
            checked={sec.auto_backup}
            onChange={(v) => update({ auto_backup: v })}
            label="启用自动备份"
          />
        </div>

        <Input
          size="lg"
          label="备份目录（留空 = 默认）"
          hint="默认位于 Hermix 数据目录下的 backups 文件夹。"
          value={sec.backup_dir}
          onChange={(e) => update({ backup_dir: e.target.value })}
          placeholder="留空使用 <数据目录>/backups"
        />

        <Input
          type="number"
          size="lg"
          min={1}
          label="备份占用上限（MB）"
          hint="超过后按时间删除最旧的备份。"
          value={sec.backup_max_mb}
          onChange={(e) => update({ backup_max_mb: Math.max(1, Number(e.target.value) || 1) })}
        />
      </Card>

      {/* 备份状态 */}
      <Card className="space-y-3">
        <div className="text-sm font-medium text-text-primary">备份状态</div>
        {!loaded ? (
          <SkeletonCard lines={2} />
        ) : (
          <div className="space-y-2 text-[length:var(--text-meta)]">
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
              className="flex items-center gap-2 px-3 py-1.5 mt-1 rounded-[var(--radius-md)] bg-bg-tertiary text-text-secondary text-[length:var(--text-meta)] transition-colors hover:bg-bg-quaternary hover:text-text-primary disabled:opacity-40"
            >
              <FolderOpen className="icon-md" /> 打开备份目录
            </button>
          </div>
        )}
      </Card>

      <Button
        variant="accent"
        onClick={handleSave}
        icon={saved ? <Check className="icon-md" /> : <Save className="icon-md" />}
      >
        {saved ? SAVED : SAVE}
      </Button>
    </div>
  );
}

function Row({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="flex items-start justify-between gap-3">
      <span className="text-text-muted shrink-0">{label}</span>
      <span className={`text-text-primary text-right break-all ${mono ? "font-mono text-[length:var(--text-meta)]" : ""}`}>
        {value}
      </span>
    </div>
  );
}
