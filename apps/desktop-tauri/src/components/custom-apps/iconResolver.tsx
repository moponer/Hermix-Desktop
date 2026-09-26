// Custom-app icon 共享解析器 — 列表卡片 (AppCard) 与详情页 (AppDetailModal)
// 共用同一套查找链路，避免两处走不一致的逻辑出现"卡片显示音符、详情页显示
// 'music' 文字"这种割裂。
//
// 查找优先级:
//   1) icon_file (后端存储的自定义 png) → <img>
//   2) icon 字段若是单 emoji (≤4 chars) → <span>{icon}</span>
//   3) icon / name 关键字命中 ICON_MAP (内置 lucide 图标库) → lucide 组件
//   4) category 命中 CATEGORY_DEFAULT (媒体/文件工具/...) → 兜底 lucide 图标
//   5) Boxes 图标 (绝对兜底)

import { useEffect, useState, type ComponentType, type ReactNode } from "react";
import { fetchJSONBinary } from "../../api/rest";
import {
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

// IconLike: 列表 (CustomAppSummary) 与详情 (CustomAppResponse.manifest) 都
// 暴露 id / name / icon / icon_file / tile_color / category / description.
// 用最小子集类型让两个调用点都能复用.
export interface IconLike {
  id: string;
  name: string;
  icon: string;
  icon_file?: string | null;
  /**
   * Tile background color (`#RRGGBB` or `#RRGGBBAA`). 仅 lucide 形态生效:
   *   - 用户上传的 `icon_file` (PNG) 已自带视觉, 不叠方块底
   *   - emoji 自身有色彩, 叠方块会变花, 跳过
   * undefined / null → 回退到 CSS 默认 (--color-bg-tertiary 灰).
   */
  tile_color?: string | null;
  category?: string;
  description?: string;
}

type IconCmp = ComponentType<LucideProps>;

// 类目兜底 lucide 图标 — 命中优先级最低, 保底保证显示语义合理.
// 注: 主人 manifest 用 "audio", 也按 media 处理 (音乐/音频同类).
const CATEGORY_DEFAULT: Record<string, IconCmp> = {
  "file-tools": FolderTree,
  "media": Music,
  "audio": Music,
  "video": Film,
  "photo": Camera,
  "productivity": Zap,
  "developer": Code2,
  "other": Boxes,
};

// 关键字 → lucide 图标 映射表. 匹配策略: 大小写不敏感, 在 name + icon
// 拼接字符串里查 substring. 仅列常见应用类型, 没命中走 category fallback,
// 再没命中走 Boxes.
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

// 保留对历史脏数据的兼容: 字符串"music"不再被当 emoji 显示.
// 检测 icon 字段是否是单 emoji (≤2 chars 主字符 + 零宽连接子).
function isEmoji(s: string): boolean {
  if (!s) return false;
  if (s.length > 4) return false;
  for (const ch of s) {
    if (ch.codePointAt(0)! > 0x2bff) return true;
    if (/[\u2600-\u27bf]/.test(ch)) return true;
  }
  return false;
}

// 在 name + id + description 拼接里查 lucide 图标.
// 优先精确匹配 manifest.icon 字段本身 (内置 lucide 库名, 如 "music"),
// 与窗口标题栏 (打包塞尾部 SVG) / 任务栏 (SVG→ICO) 同源, 保证三处一致.
function lookupIcon(app: IconLike): IconCmp | null {
  const iconName = (app.icon ?? "").trim().toLowerCase();
  if (iconName) {
    const exact = ICON_MAP[iconName];
    if (exact) return exact;
  }
  const haystack = `${app.name} ${app.id} ${app.description ?? ""}`.toLowerCase();
  for (const [key, Icon] of Object.entries(ICON_MAP)) {
    if (haystack.includes(key)) return Icon;
  }
  return null;
}

export interface ResolveIconOpts {
  // 给非 img 形态 (emoji span / lucide span) 套的 class, 列表卡片与
  // 详情头部用不同 class, 让调用方各自管布局/尺寸.
  spanClassName?: string;
  // 给 img 形态的 class
  imgClassName?: string;
  // lucide 图标尺寸 (默认 20, 详情头部可能想更大)
  size?: number;
  // lucide 描边粗细
  strokeWidth?: number;
}

/**
 * 最终的 lucide 形态渲染 — emoji 与 PNG 之后的兜底。
 * tile_color 存在时强制固定方块色 + 白前景, 不再让 CSS hover 覆盖
 * (主人要"背景不透明", 行为应该稳定); 不存在时回退到 CSS 默认灰底.
 */
function renderLucideFallback(app: IconLike, opts: ResolveIconOpts): ReactNode {
  const { spanClassName, size = 20, strokeWidth = 1.75 } = opts;
  const LucideIcon = lookupIcon(app) ?? CATEGORY_DEFAULT[app.category ?? ""] ?? Boxes;
  const tile = app.tile_color?.trim();
  const spanStyle: React.CSSProperties | undefined = tile
    ? { backgroundColor: tile, color: "#ffffff" }
    : undefined;
  return (
    <span className={spanClassName} style={spanStyle} aria-hidden="true">
      <LucideIcon size={size} strokeWidth={strokeWidth} />
    </span>
  );
}

/**
 * 自定义 PNG 图标。manifest 声明了 icon_file 但文件实际不存在时,
 * /api/custom-apps/{id}/icon 会 404 —— 此时降级到 lucide 兜底, 保证任何
 * 情况下都有一个可辨识的图标。
 *
 * 图标字节通过 stdio bridge 的 `fetchJSONBinary` 拉取 (enc=base64 回传),
 * 渲染成 objectURL 喂给 <img>。生产环境 <img src> 直接填后端路径会解析
 * 到 Tauri 内部 URL → 永远 404 → 上传的 PNG 图标全部静默失效。
 * objectURL 在卸载 / 降级时 revoke, 防内存泄漏。
 */
function IconImage({ app, opts }: { app: IconLike; opts: ResolveIconOpts }) {
  const [objectUrl, setObjectUrl] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    let url: string | null = null;

    void (async () => {
      try {
        const blob = await fetchJSONBinary(
          "GET",
          `/api/custom-apps/${encodeURIComponent(app.id)}/icon`,
        );
        if (cancelled) {
          URL.revokeObjectURL(url ?? "");
          return;
        }
        url = URL.createObjectURL(blob);
        setObjectUrl(url);
      } catch {
        if (!cancelled) setFailed(true);
      }
    })();

    return () => {
      cancelled = true;
      if (url) URL.revokeObjectURL(url);
    };
  }, [app.id, app.icon_file]);

  if (failed) return renderLucideFallback(app, opts);
  if (!objectUrl) return renderLucideFallback(app, opts);
  return (
    <img
      className={opts.imgClassName}
      src={objectUrl}
      alt={app.name}
      draggable={false}
    />
  );
}

export function resolveAppIconEl(app: IconLike, opts: ResolveIconOpts = {}): ReactNode {
  const { spanClassName } = opts;

  // 用户上传的 PNG 自带完整视觉, 不叠 tile_color (否则会把 PNG 盖掉).
  if (app.icon_file) {
    return <IconImage app={app} opts={opts} />;
  }
  // emoji 自身有色彩 + 字体渲染, 叠方块底会变花 (主人拍板跳过).
  if (isEmoji(app.icon)) {
    return <span className={spanClassName}>{app.icon}</span>;
  }
  return renderLucideFallback(app, opts);
}
