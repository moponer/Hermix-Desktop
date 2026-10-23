// SkillCard.tsx —— 技能卡 + 内容阅读弹窗（R2-4 拆分第三步，从 ToolTabs 抽出）。
import { useState, useCallback } from "react";
import { Wand2, Sparkles } from "lucide-react";
import * as api from "../../api";
import type { SkillInfo } from "../../api";
import { ToolCardIcon } from "./ToolCardIcon";
import { ToolListRow } from "./ToolListRow";
import { ConfirmDialog, Modal, SkeletonCard } from "../ui";
import { ToggleSwitch } from "../ToggleSwitch";
import { MarkdownBody } from "../MarkdownBody";

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
      <ToolListRow
        onClick={openContent}
        icon={
          <ToolCardIcon category={skill.category}>
            <Wand2 className="icon-sm" />
          </ToolCardIcon>
        }
        title={skill.display_name || skill.name}
        meta={catLabel(skill.category) || "skill"}
        description={skill.description || "（无描述）"}
        action={
          <ToggleSwitch
            enabled={skill.enabled}
            onToggle={requestToggle}
            disabled={toggling}
            offLabel={`停用 ${skill.name}`}
            onLabel={`启用 ${skill.name}`}
          />
        }
      />

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
              需要重新下载才能再次启用。
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
  return (
    /* 弹窗壳收编到 ui/Modal：焦点陷阱 / Esc / 焦点归还 / role=dialog +
       aria-modal 一次性补齐（此前这个弹窗只有手写的 Esc 监听）。
       头部不再自带一层底色 —— 头部/正文的层次由共享壳统一给出。 */
    <Modal
      open
      onClose={onClose}
      width="wide"
      scrim
      title={
        <span className="flex items-center gap-3">
          {/* 图标与列表卡共用 ToolCardIcon —— 此前这里用的是 .modal-icon-info
              的灰色块，与卡片里的主色图标对不上，"图标没有颜色"。那个组件存在的
              全部意义就是让四页图标必然一致，弹窗没走它就又漂了一处。 */}
          <ToolCardIcon category={skill.category}>
            <Sparkles className="icon-lg" />
          </ToolCardIcon>
          <span className="flex min-w-0 flex-col gap-0.5">
            <span className="truncate">{skill.display_name || skill.name}</span>
            <span className="line-clamp-2 text-[length:var(--text-meta)] font-normal text-[var(--color-text-muted)]">
              {catLabel(skill.category)}
              {skill.description ? ` · ${skill.description}` : ""}
            </span>
          </span>
        </span>
      }
      bodyClassName="overflow-y-auto bg-[var(--color-bg-elevated)] px-5 py-4"
    >
      {/* 正文走 MarkdownBody：SKILL.md 本来就是 markdown，用 <pre> 平铺等于
          把标题层级、有序列表、行内代码全部抹平——「界面没有层次」的一半在
          这里，另一半在头部那层底色。 */}
      {loading ? (
        <SkeletonCard lines={6} />
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
    </Modal>
  );
}
