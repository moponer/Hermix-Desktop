// AppCard — displays a single custom app in the grid view.
//
// 图标优先级:
//   1) icon_file (后端存储的自定义 png) → 直接渲染
//   2) icon 字段若是单 emoji (≤2 chars) → 当 emoji 显示 (历史数据兼容)
//   3) icon / name 关键字命中 ICON_MAP (内置 lucide 图标库) → 渲染 lucide 图标
//   4) category 命中 CATEGORY_DEFAULT (媒体/文件工具/...) → 兜底 lucide 图标
//   5) Boxes 图标 (绝对兜底)
//
// 之前的设计: 直接把 app.icon 当文本塞到 28px 容器, 用户填 "music" 就显示大字"music"
//             挤压布局、字面意义缺失、违反"内置图标库"的设计意图

import type { ComponentType, MouseEvent } from "react";
import {
  Package,
  FolderOpen,
  Play,
  Trash2,
  Eye,
  Music,
  Music2,
  Music3,
  Music4,
  FileText,
  Files,
  FolderTree,
  Folder,
  Image as ImageIcon,
  Film,
  Camera,
  Code,
  Code2,
  Terminal,
  Bug,
  GitBranch,
  Bot,
  Boxes,
  Zap,
  Calendar,
  Clock,
  Calculator,
  StickyNote,
  NotebookPen,
  Search,
  Settings,
  Database,
  Server,
  Cloud,
  Globe,
  Mail,
  MessageSquare,
  Newspaper,
  BookOpen,
  Languages,
  Mic,
  Headphones,
  Radio,
  Tv,
  Gamepad2,
  Palette,
  Brush,
  PenTool,
  Sparkles,
  Wand2,
  Hammer,
  Wrench,
  Cog,
  Plug,
  Lightbulb,
  GraduationCap,
  Briefcase,
  PieChart,
  BarChart3,
  TrendingUp,
  Wallet,
  CreditCard,
  ShoppingCart,
  ShoppingBag,
  Heart,
  Star,
  Bookmark,
  Tag,
  Map,
  MapPin,
  Compass,
  Plane,
  Car,
  Bike,
  Ship,
  Anchor,
  Trophy,
  Award,
  Target,
  type LucideProps,
} from "lucide-react";
import type { CustomAppSummary } from "../../lib/customAppApi";

interface AppCardProps {
  app: CustomAppSummary;
  onView: () => void;
  onPackage: () => void;
  onOpenDist: () => void;
  onRun: () => void;
  onDelete: () => void;
  packaging?: boolean;
}

// 防冒泡：右上角 icon 按钮点击时不应触发 header 的 onView（查看详情）
const stopProp = (fn: () => void) => (e: MouseEvent) => {
  e.stopPropagation();
  fn();
};

const CATEGORY_LABELS: Record<string, string> = {
  "file-tools": "文件工具",
  "media": "媒体",
  "productivity": "效率工具",
  "developer": "开发工具",
  "other": "其他",
};

// 类目兜底 lucide 图标 — 命中优先级最低, 保底保证显示语义合理
type IconCmp = ComponentType<LucideProps>;
const CATEGORY_DEFAULT: Record<string, IconCmp> = {
  "file-tools": FolderTree,
  "media": Music,
  "productivity": Zap,
  "developer": Code2,
  "other": Boxes,
};

// 关键字 → lucide 图标 映射表
// 匹配策略: 大小写不敏感, 在 name + icon 拼接字符串里查 substring
// 仅列常见应用类型, 没命中走 category fallback, 再没命中走 Boxes
const ICON_MAP: Record<string, IconCmp> = {
  // 媒体
  music: Music, song: Music, audio: Music, sound: Music2,
  playlist: Music3, playlist2: Music4, album: Music4,
  video: Film, movie: Film, film: Film,
  tv: Tv, television: Tv, live: Radio, radio: Radio, podcast: Mic,
  microphone: Mic, mic: Mic, record: Mic,
  headphones: Headphones, headset: Headphones, earphone: Headphones,
  photo: Camera, photograph: Camera, camera: Camera, screenshot: Camera,
  image: ImageIcon, picture: ImageIcon, gallery: ImageIcon,
  // 文件工具 / 笔记 / 知识
  file: FileText, document: FileText, doc: FileText, txt: FileText,
  files: Files, documents: Files,
  folder: Folder, directory: Folder,
  note: StickyNote, notes: NotebookPen, memo: StickyNote,
  markdown: BookOpen, blog: BookOpen, article: Newspaper,
  book: BookOpen, ebook: BookOpen, reader: BookOpen,
  translate: Languages, language: Languages, dict: Languages,
  search: Search, find: Search,
  calendar: Calendar, schedule: Calendar, agenda: Calendar,
  clock: Clock, timer: Clock, alarm: Clock,
  count: Calculator, calculator: Calculator, math: Calculator,
  // 效率
  task: Zap, todo: Zap, productivity: Zap,
  idea: Lightbulb, think: Lightbulb,
  // 开发
  code: Code, source: Code, programming: Code,
  dev: Code2, developer: Code2,
  terminal: Terminal, shell: Terminal, bash: Terminal, cmd: Terminal,
  bug: Bug, debug: Bug, fix: Bug,
  git: GitBranch, repo: GitBranch,
  database: Database, db: Database, sql: Database,
  api: Server, server: Server, backend: Server,
  cloud: Cloud, deploy: Cloud,
  web: Globe, browser: Globe, internet: Globe,
  // 通讯
  mail: Mail, email: Mail, inbox: Mail,
  message: MessageSquare, chat: MessageSquare,
  news: Newspaper, rss: Newspaper, feed: Newspaper,
  // AI / 助手
  ai: Bot, bot: Bot, agent: Bot, assistant: Bot,
  spark: Sparkles, magic: Wand2, generate: Sparkles,
  // 设计 / 创作
  draw: PenTool, design: PenTool, pen: PenTool,
  paint: Palette, art: Palette,
  brush: Brush,
  // 工具 / 系统
  settings: Settings, config: Settings, preference: Settings,
  tool: Wrench, tools: Hammer,
  build: Hammer, project: Briefcase,
  cog: Cog, gear: Cog, options: Settings,
  plug: Plug, integrate: Plug, integration: Plug,
  // 数据可视化
  chart: PieChart, graph: PieChart, plot: BarChart3,
  analytics: TrendingUp, trend: TrendingUp,
  // 财务 / 购物
  money: Wallet, wallet: Wallet, finance: Wallet,
  payment: CreditCard, credit: CreditCard, card: CreditCard,
  shop: ShoppingCart, store: ShoppingBag, cart: ShoppingCart,
  // 生活
  heart: Heart, favorite: Heart, like: Heart,
  star: Star, rate: Star,
  bookmark: Bookmark, save: Bookmark,
  tag: Tag, label: Tag,
  map: Map, maps: Map, location: MapPin, place: MapPin,
  travel: Plane, trip: Plane,
  car: Car, drive: Car,
  bike: Bike, bicycle: Bike,
  ship: Ship, boat: Ship,
  // 学习 / 游戏
  learn: GraduationCap, study: GraduationCap, school: GraduationCap,
  game: Gamepad2, play: Gamepad2,
  // 杂项 / 兜底
  trophy: Trophy, award: Award, prize: Award,
  target: Target, goal: Target,
  anchor: Anchor,
  compass: Compass,
};

// 检测 icon 字段是否是单 emoji (保留对历史脏数据的兼容: 字符串"music"不再被当 emoji 显示)
function isEmoji(s: string): boolean {
  if (!s) return false;
  // 限制长度以保留对少量单 emoji 的兼容
  if (s.length > 4) return false;
  // 任意码点 > 0x2BFF 视为非 BMP 字符 (含 emoji)
  for (const ch of s) {
    if (ch.codePointAt(0)! > 0x2bff) return true;
    // 部分 BMP 区的符号字符如 ★ ☆ ❤ ⚡ 也算
    if (/[\u2600-\u27bf]/.test(ch)) return true;
  }
  return false;
}

// 在 name + icon 拼接里查 lucide 图标
function lookupIcon(app: CustomAppSummary): IconCmp | null {
  // 1. 优先精确匹配 manifest.icon 字段本身 (内置 lucide 库名, 如 "music") —
  //    与窗口标题栏 (打包塞尾部 SVG) / 任务栏 (SVG→ICO) 同源, 保证三处一致.
  const iconName = (app.icon ?? "").trim().toLowerCase();
  if (iconName) {
    const exact = ICON_MAP[iconName];
    if (exact) return exact;
  }
  // 2. fallback: name + id + description 关键字
  const haystack = `${app.name} ${app.id} ${app.description}`.toLowerCase();
  for (const [key, Icon] of Object.entries(ICON_MAP)) {
    if (haystack.includes(key)) return Icon;
  }
  return null;
}

export function AppCard({ app, onView, onPackage, onOpenDist, onRun, onDelete, packaging }: AppCardProps) {
  // 图标优先级 (见文件头注释)
  const iconEl = (() => {
    if (app.icon_file) {
      return (
        <img
          className="custom-app-card__icon-img"
          src={`/api/custom-apps/${encodeURIComponent(app.id)}/icon`}
          alt={app.name}
          draggable={false}
        />
      );
    }
    if (isEmoji(app.icon)) {
      return <span className="custom-app-card__icon">{app.icon}</span>;
    }
    const LucideIcon = lookupIcon(app) ?? CATEGORY_DEFAULT[app.category] ?? Boxes;
    return (
      <span className="custom-app-card__icon" aria-hidden="true">
        <LucideIcon size={20} strokeWidth={1.75} />
      </span>
    );
  })();

  return (
    <div className="custom-app-card group">
      <div className="custom-app-card__header" onClick={onView}>
        {iconEl}
        <div className="custom-app-card__info">
          <h3 className="custom-app-card__name">{app.name}</h3>
          <span className="custom-app-card__version">v{app.version}</span>
        </div>
        {/* 右上角：已打包时显示徽章 + 打开目录 + 重新打包 (icon-only 紧凑) */}
        {app.is_packaged && (
          <div className="custom-app-card__header-actions">
            <span className="custom-app-card__badge custom-app-card__badge--packaged">
              已打包
            </span>
            <button
              className="custom-app-card__icon-btn"
              onClick={stopProp(onOpenDist)}
              title="打开打包后的应用目录"
            >
              <FolderOpen size={14} />
            </button>
            <button
              className="custom-app-card__icon-btn custom-app-card__icon-btn--package"
              onClick={stopProp(onPackage)}
              disabled={packaging}
              title={
                packaging
                  ? "打包中…"
                  : "重新打包（用最新 hx-shell 壳覆盖 dist）"
              }
            >
              <Package size={14} />
            </button>
          </div>
        )}
      </div>

      <p className="custom-app-card__description" onClick={onView}>
        {app.description}
      </p>

      <div className="custom-app-card__meta">
        <span className="custom-app-card__category">
          {CATEGORY_LABELS[app.category] ?? app.category}
        </span>
        <span className="custom-app-card__author">by {app.author}</span>
      </div>

      <div className="custom-app-card__actions">
        <button
          className="custom-app-card__btn custom-app-card__btn--view"
          onClick={onView}
          title="查看详情"
        >
          <Eye size={14} />
          详情
        </button>
        <button
          className="custom-app-card__btn custom-app-card__btn--run"
          onClick={onRun}
          title="运行（自动编译并启动，用于本地测试）"
        >
          <Play size={14} />
          运行
        </button>
        {/* 未打包：在底部显示"打包"按钮（已打包的状态下，"打开目录"和"重新打包"挪到了右上角） */}
        {!app.is_packaged && (
          <button
            className="custom-app-card__btn custom-app-card__btn--package"
            onClick={onPackage}
            disabled={packaging}
            title="打包为可分发到其他电脑的独立 EXE"
          >
            <Package size={14} />
            {packaging ? "打包中..." : "打包"}
          </button>
        )}
        <button
          className="custom-app-card__btn custom-app-card__btn--delete"
          onClick={onDelete}
          title="删除应用"
        >
          <Trash2 size={14} />
        </button>
      </div>
    </div>
  );
}
