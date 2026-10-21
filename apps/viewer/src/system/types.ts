// 系统资源管理的类型定义 —— 与 Rust 侧 serde(camelCase) 一一对应。
//
// 单独一个文件：清理页、索引页、进程页都要用同一份契约，写三遍必然对不上。

// ── 磁盘 ──────────────────────────────────────────────────────────────

export interface DiskRow {
  name: string;
  mount: string;
  totalBytes: number;
  freeBytes: number;
  removable: boolean;
  drive: string;
}

// ── 垃圾扫描（规则驱动） ─────────────────────────────────────────────

export interface CleanItem {
  id: string;
  title: string;
  /** 清理依据：为什么这东西可以删。 */
  hint: string;
  /** 分类：UI 按它分组。 */
  group: string;
  /** 规则出处（开源清理工具里的同名规则）。 */
  source: string;
  bytes: number;
  files: number;
  risky: boolean;
  needsAdmin: boolean;
  roots: string[];
  /** 展开出的目标总数（`roots` 只带前几个预览）。 */
  rootsTotal: number;
  available: boolean;
}

export interface ScanReport {
  items: CleanItem[];
  totalBytes: number;
  totalFiles: number;
  elapsedMs: number;
  drive: string | null;
  /** 扫描预算耗尽，结果不完整。 */
  truncated: boolean;
}

export interface CleanResult {
  freedBytes: number;
  deleted: number;
  skipped: number;
  failed: number;
  messages: string[];
}

// ── 盘内扫描（建议清理 / 大文件） ────────────────────────────────────

export interface CleanupCandidate {
  path: string;
  /** 展示类别：构建产物 / 临时残留 / 日志与转储 / 残留安装包。 */
  category: string;
  /** 清理依据（UI 必须明示）。 */
  reason: string;
  /** `artifact` = 删整个目录；`file` = 只删这个文件。 */
  target: string;
  bytes: number;
  files: number;
  modified: number;
}

export interface FoundFile {
  path: string;
  bytes: number;
  modified: number;
  reason: string;
}

export interface DirUsage {
  path: string;
  parent: string;
  bytes: number;
  files: number;
  depth: number;
}

export interface DriveScanReport {
  drive: string;
  /** 「建议清理」：规则命中的可删除项。 */
  suggested: CleanupCandidate[];
  /** 建议清理命中总数（含未列出的）。 */
  suggestedTotal: number;
  /** 「大文件」：仅作参考，按体积降序。 */
  big: FoundFile[];
  emptyDirs: string[];
  emptyDirCount: number;
  dirs: DirUsage[];
  dirsScanned: number;
  elapsedMs: number;
  truncated: boolean;
}

export interface RemoveResult {
  freedBytes: number;
  removed: number;
  failed: number;
  messages: string[];
}

// ── 进度事件 ──────────────────────────────────────────────────────────

export interface CleanupProgress {
  phase: "scan" | "clean" | "drive";
  ruleId?: string;
  title?: string;
  done: number;
  total: number;
  bytes?: number;
  files?: number;
  deleted?: number;
  freedBytes?: number;
  skipped?: number;
  /** phase=drive 时的当前目录与已用时。 */
  path?: string;
  elapsedMs?: number;
}

// ── 文件索引 ──────────────────────────────────────────────────────────

export type MonitorState =
  | "Idle"
  | "Watching"
  | "Syncing"
  | "NeedsRebuild"
  | "Error";

export interface BuildProgress {
  volume: string;
  /** scanning（枚举 MFT）/ sizing（读体积）/ indexing（建表）/ saving（落盘）。 */
  phase: string;
  entries: number;
  bytes: number;
  elapsedMs: number;
  done: boolean;
  error?: string | null;
}

export interface IndexStatus {
  root?: string | null;
  volume?: string | null;
  totalEntries: number;
  /** 已索引的文件体积之和（字节）。 */
  indexedBytes: number;
  isIndexed: boolean;
  lastScanMs?: number | null;
  snapshotLoaded: boolean;
  snapshotPath?: string | null;
  journalId?: number | null;
  nextUsn?: number | null;
  indexSource?: string | null;
  needsRebuild: boolean;
  statusMessage?: string | null;
  monitorState: MonitorState;
  lastSyncAt?: number | null;
  lastSyncError?: string | null;
  build?: BuildProgress | null;
}

export interface ServiceStatus {
  volumes: IndexStatus[];
  skipped: Record<string, string>;
}

export interface FilesearchStatus {
  running: boolean;
  reason?: string;
  status?: ServiceStatus;
}

// ── 进程 ──────────────────────────────────────────────────────────────

export interface ProcessRow {
  pid: number;
  parentPid: number | null;
  name: string;
  exe: string | null;
  cmd: string;
  cpu: number;
  memoryMb: number;
  user: string | null;
  status: string;
  startedAt: number;
  description: string | null;
  company: string | null;
  critical: boolean;
}

export interface ProcessSnapshot {
  processes: ProcessRow[];
  overview: {
    totalMemoryMb: number;
    usedMemoryMb: number;
    cpuCount: number;
    processCount: number;
    cpuUsage: number;
    uptimeSecs: number;
    hostName: string;
    osVersion: string;
  };
}
