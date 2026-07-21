// AppCard — displays a single custom app in the grid view

import { Package, Play, Trash2, Eye } from "lucide-react";
import type { CustomAppSummary } from "../../lib/customAppApi";

interface AppCardProps {
  app: CustomAppSummary;
  onView: () => void;
  onPackage: () => void;
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

export function AppCard({ app, onView, onPackage, onRun, onDelete, packaging }: AppCardProps) {
  // Determine the icon to display: custom PNG image or emoji fallback
  const iconEl = app.icon_file ? (
    <img
      className="custom-app-card__icon-img"
      src={`/api/custom-apps/${encodeURIComponent(app.id)}/icon`}
      alt={app.name}
      draggable={false}
    />
  ) : (
    <span className="custom-app-card__icon">{app.icon}</span>
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
        {app.is_packaged ? (
          <button
            className="custom-app-card__btn custom-app-card__btn--run"
            onClick={onRun}
            title="启动应用"
          >
            <Play size={14} />
            启动
          </button>
        ) : (
          <button
            className="custom-app-card__btn custom-app-card__btn--package"
            onClick={onPackage}
            disabled={packaging}
            title="打包为独立 EXE"
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
