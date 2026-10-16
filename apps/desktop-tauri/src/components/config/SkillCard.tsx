// SkillCard.tsx —— 技能卡 + 内容阅读弹窗（R2-4 拆分第三步，从 ToolTabs 抽出）。
import { useState, useCallback, useEffect } from "react";
import { Wand2, ArrowUpRight, Sparkles, X } from "lucide-react";
import * as api from "../../api";
import type { SkillInfo } from "../../api";
import { ToolCardIcon } from "./ToolCardIcon";
import { MgmtTag } from "./MgmtTag";
import { ToggleSwitch } from "../ToggleSwitch";
import { MarkdownBody } from "../MarkdownBody";
import { ConfirmDialog } from "../ui";

export function SkillCard({ skill, catLabel, onToggle }: {
  skill: SkillInfo;
  catLabel: (key: string) => string;
  onToggle?: (name: string) => void;
}) {
  const [showModal, setShowModal] = useState(false);
  const [content, setContent] = useState<string | null>(null);
  const [loadingContent, setLoadingContent] = useState(false);
  const [toggling, setToggling] = useState(false);
  const [pendingDisable, setPendingDisable] = useState(false);

  const openContent = useCallback(async () => {
    setShowModal(true);
    if (content === null) {
      setLoadingContent(true);
      try {
        const c = await api.getSkillContent(skill.name);
        if (c) setContent(c.content);
      } catch {
        // stay null
      } finally {
        setLoadingContent(false);
      }
    }
  }, [content, skill.name]);

  const closeContent = useCallback(() => setShowModal(false), []);

  const doToggle = useCallback(async () => {
    setToggling(true);
    try {
      await api.toggleSkill(skill.name, !skill.enabled);
      onToggle?.(skill.name);
    } catch {
      // silently fail
    } finally {
      setToggling(false);
    }
  }, [skill, onToggle]);

  const requestToggle = useCallback(() => {
    if (skill.enabled) {
      setPendingDisable(true);
    } else {
      void doToggle();
    }
  }, [skill.enabled, doToggle]);

  const confirmDisable = useCallback(() => {
    setPendingDisable(false);
    void doToggle();
  }, [doToggle]);

  return (
    <>
      <div className="group rounded-xl bg-bg-card hover:bg-bg-card-hover transition-colors">
        <div className="flex items-center gap-3 px-3 py-2.5">
          <ToolCardIcon onClick={openContent} title="查看技能内容" aria-label="查看技能内容">
            <Wand2 className="icon-md" />
          </ToolCardIcon>
          <div className="min-w-0 flex-1 cursor-pointer" onClick={openContent}>
            <div className="flex items-center gap-2">
              <span className="text-[length:var(--text-ui)] font-medium text-text-primary truncate">
                {skill.display_name || skill.name}
              </span>
              <MgmtTag tone="neutral" className="shrink-0">
                {catLabel(skill.category) || "skill"}
              </MgmtTag>
            </div>
            <p className="text-[length:var(--text-meta)] text-text-muted mt-0.5 line-clamp-2 group-hover:line-clamp-none">
              {skill.description || "（无描述）"}
            </p>
          </div>
          <ArrowUpRight className="icon-sm text-text-faint shrink-0" />
          <ToggleSwitch enabled={skill.enabled} onToggle={requestToggle} disabled={toggling} offLabel={`停用 ${skill.name}`} onLabel={`启用 ${skill.name}`} className="shrink-0" />
        </div>
      </div>

      {showModal && (
        <SkillContentModal
          skill={skill}
          catLabel={catLabel}
          content={content}
          loading={loadingContent}
          onClose={closeContent}
        />
      )}

      {pendingDisable && (
        <ConfirmDialog
          open
          title="禁用技能"
          body={
            <>
              即将从文件系统中删除技能 <span className="font-medium text-text-primary">{skill.name}</span>。
              该操作不可撤销，需要重新下载才能再次启用。
            </>
          }
          confirmLabel="禁用并删除"
          busy={toggling}
          onCancel={() => setPendingDisable(false)}
          onConfirm={confirmDisable}
        />
      )}
    </>
  );
}

// ── SkillContentModal: popup for reading skill markdown ─────────

export function SkillContentModal({ skill, catLabel, content, loading, onClose }: {
  skill: SkillInfo;
  catLabel: (key: string) => string;
  content: string | null;
  loading: boolean;
  onClose: () => void;
}) {
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        className="modal-card modal-card-lg"
        onClick={(e) => e.stopPropagation()}
        style={{ maxHeight: "80vh", display: "flex", flexDirection: "column" }}
      >
        {/* 头部自带一层底色，与正文分开 —— 层次靠底色差，不靠描边。
            标题与描述都限高：此前两者都不限，一段长描述会把整个头撑成十几行、
            把关闭按钮顶到视线之外（"过长的标题会改变显示"就是这个）。 */}
        <div
          className="modal-header"
          style={{ flexShrink: 0, background: "var(--color-bg-secondary)" }}
        >
          {/* 图标与列表卡共用 ToolCardIcon —— 此前这里用的是 .modal-icon-info
              的灰色块，与卡片里的主色图标对不上，"图标没有颜色"。那个组件存在的
              全部意义就是让四页图标必然一致，弹窗没走它就又漂了一处。 */}
          <ToolCardIcon>
            <Sparkles className="icon-lg" />
          </ToolCardIcon>
          <div className="modal-title-block">
            <h3 className="modal-title truncate" title={skill.display_name || skill.name}>
              {skill.display_name || skill.name}
            </h3>
            <p className="modal-subtitle line-clamp-2">
              {catLabel(skill.category)}
              {skill.description ? ` · ${skill.description}` : ""}
            </p>
          </div>
          <button onClick={onClose} className="modal-close shrink-0" aria-label="关闭">
            <X className="icon-lg" />
          </button>
        </div>
        {/* 正文走 MarkdownBody：SKILL.md 本来就是 markdown，用 <pre> 平铺等于
            把标题层级、有序列表、行内代码全部抹平——「界面没有层次」的一半在
            这里，另一半在头部那层底色。 */}
        <div className="flex-1 overflow-auto bg-bg-elevated px-5 py-4">
          {loading ? (
            <div className="text-[length:var(--text-meta)] text-text-muted animate-pulse">
              加载中…
            </div>
          ) : content ? (
            <MarkdownBody content={content} />
          ) : (
            <div className="text-[length:var(--text-meta)] text-text-muted">
              <p>无法加载技能内容</p>
              <p className="mt-2 opacity-60 break-all">
                存放路径：~/.hermix/skills/{skill.category}/{skill.name}/SKILL.md
              </p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
