// AppCard — displays a single custom app in the grid view

import { Package, FolderOpen, Play, Trash2, Eye } from "lucide-react";
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

const CATEGORY_LABELS: Record<string, string> = {
  "file-tools": "文件工具",
  "media": "媒体",
  "productivity": "效率工具",
  "developer": "开发工具",
  "other": "其他",
};

export function AppCard({ app, onView, onPackage, onOpenDist, onRun, onDelete, packaging }: AppCardProps) {
  // 主程序列表优先显示 emoji 徽标 (保持整体 UI 风格一致);
  // 生成的 PNG 图标专用于独立壳窗口, 这里仅作 emoji 缺失时的兜底。
  const iconEl = app.icon ? (
    <span className="custom-app-card__icon">{app.icon}</span>
  ) : app.icon_file ? (
    <img
      className="custom-app-card__icon-img"
      src={`/api/custom-apps/${encodeURIComponent(app.id)}/icon`}
      alt={app.name}
      draggable={false}
    />
  ) : (
    <span className="custom-app-card__icon">📦</span>
  );

  return (
    <div className="custom-app-card group">
      <div className="custom-app-card__header" onClick={onView}>
        {iconEl}
        <div className="custom-app-card__info">
          <h3 className="custom-app-card__name">{app.name}</h3>
          <span className="custom-app-card__version">v{app.version}</span>
        </div>
        {app.is_packaged && (
          <span className="custom-app-card__badge custom-app-card__badge--packaged">
            已打包
          </span>
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
        {app.is_packaged ? (
          <button
            className="custom-app-card__btn custom-app-card__btn--open"
            onClick={onOpenDist}
            title="打开打包后的应用目录"
          >
            <FolderOpen size={14} />
            打开目录
          </button>
        ) : (
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
