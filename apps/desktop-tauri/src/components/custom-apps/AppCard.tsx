// AppCard — displays a single custom app in the grid view.
//
// 图标解析 (lookupIcon / isEmoji / CATEGORY_DEFAULT / ICON_MAP) 抽到
// ./iconResolver.tsx 共享, 保证与详情页 AppDetailModal 头部 icon 走同一套
// 查找链路 (避免"卡片显示 ♪ 音符, 详情页头部显示 'music' 文字"的割裂).

import {
  Package,
  FolderOpen,
  Play,
  Trash2,
  RefreshCw,
} from "lucide-react";
import type { CustomAppSummary } from "../../lib/customAppApi";
import { formatCategory } from "./labels";
import { resolveAppIconEl } from "./iconResolver";

interface AppCardProps {
  app: CustomAppSummary;
  onView: () => void;
  onPackage: () => void;
  onOpenDist: () => void;
  onRun: () => void;
  onDelete: () => void;
  packaging?: boolean;
}

export function AppCard({ app, onView, onPackage, onOpenDist, onRun, onDelete, packaging }: AppCardProps) {
  const iconEl = resolveAppIconEl(app, {
    spanClassName: "custom-app-card__icon",
    imgClassName: "custom-app-card__icon-img",
  });

  return (
    <div className="custom-app-card group">
      <div className="custom-app-card__header" onClick={onView}>
        {iconEl}
        <div className="custom-app-card__info">
          <div className="custom-app-card__name-row">
            <h3 className="custom-app-card__name">{app.name}</h3>
            <span className="custom-app-card__version">v{app.version}</span>
            {app.is_packaged && (
              <span className="custom-app-card__badge custom-app-card__badge--packaged">
                已打包
              </span>
            )}
          </div>
        </div>
      </div>

      <p className="custom-app-card__description" onClick={onView}>
        {app.description}
      </p>

      <div className="custom-app-card__meta">
        <span className="custom-app-card__category">{formatCategory(app.category)}</span>
        <span className="custom-app-card__author">by {app.author}</span>
      </div>

      <div className="custom-app-card__actions">
        <button
          className="custom-app-card__btn custom-app-card__btn--run"
          onClick={onRun}
          title="运行（自动编译并启动，用于本地测试）"
        >
          <Play className="icon-md" />
          运行
        </button>
        {app.is_packaged ? (
          <>
            <button
              className="custom-app-card__btn custom-app-card__btn--package"
              onClick={onPackage}
              disabled={packaging}
              title="重新打包为可分发到其他电脑的独立 EXE"
            >
              <RefreshCw className="icon-md" />
              {packaging ? "打包中..." : "打包"}
            </button>
            <button
              className="custom-app-card__btn custom-app-card__btn--secondary"
              onClick={onOpenDist}
              title="打开打包后的应用目录"
            >
              <FolderOpen className="icon-md" />
              打开目录
            </button>
          </>
        ) : (
          <button
            className="custom-app-card__btn custom-app-card__btn--package"
            onClick={onPackage}
            disabled={packaging}
            title="打包为可分发到其他电脑的独立 EXE"
          >
            <Package className="icon-md" />
            {packaging ? "打包中..." : "打包"}
          </button>
        )}
        <button
          className="custom-app-card__btn custom-app-card__btn--delete"
          onClick={onDelete}
          title="删除应用"
        >
          <Trash2 className="icon-md" />
        </button>
      </div>
    </div>
  );
}
