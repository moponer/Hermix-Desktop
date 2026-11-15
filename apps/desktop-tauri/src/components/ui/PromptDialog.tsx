import { useEffect, useState, type ReactNode } from "react";

import { cx } from "../../lib/cx";
import { Button } from "./Button";
import { Input } from "./Input";
import { Modal } from "./Modal";
import { CANCEL, CONFIRM } from "../strings";

export interface PromptDialogProps {
  open: boolean;
  /** 标题（一句话说清在改什么，如「重命名会话」）。 */
  title: ReactNode;
  /** 输入框的可访问名（视觉上隐藏，标题已经表达了意图）。 */
  label: string;
  placeholder?: string;
  /** 标题左侧的语义图标（`Pencil` / `Plus` …）。色调固定 accent。 */
  icon?: ReactNode;
  /** 打开时填入的初始值。每次 `open` 变 true 都会重置。 */
  initialValue?: string;
  confirmLabel?: string;
  cancelLabel?: string;
  /**
   * 额外校验：返回字符串表示错误（禁用确认并显示），返回 null 表示通过。
   * 空值拦截由组件负责，这里只处理业务规则（重名、长度、非法字符…）。
   */
  validate?: (value: string) => string | null;
  onSubmit: (value: string) => void;
  onCancel: () => void;
}

/**
 * 单行文本输入弹窗（重命名 / 新建分组 / 填一个名字）。
 *
 * 与 `ConfirmDialog` **同构**：左侧语义图标 + 标题 + 字段 + 底栏
 * （取消在左、确认在右，底栏靠 `bg-secondary` 亮度差分块）。
 *
 * 2026-10-23 新建：此前 Sidebar 里有一份私有的 `RenameDialog`，
 * 自建了遮罩、容器、输入框和按钮，问题是
 *   · 容器带 `border`（描边）与全站无描边的语言冲突；
 *   · 按钮顺序是「确认 | 取消」——反的，和全站 11 处确认框相反；
 *   · 输入框是手写的 `focus:ring-1`，与 `ui/Input` 的 2px accent 环不一致；
 *   · 标题与输入框之间只有 8px，没有层次。
 * 这类"每个文件自己写一个弹窗"是本项目反复回归的根源，故收编为公共件。
 */
export function PromptDialog({
  open,
  title,
  label,
  placeholder,
  icon,
  initialValue = "",
  confirmLabel = CONFIRM,
  cancelLabel = CANCEL,
  validate,
  onSubmit,
  onCancel,
}: PromptDialogProps) {
  const [value, setValue] = useState(initialValue);
  const [touched, setTouched] = useState(false);

  // 每次打开都重置：否则第二次打开会带着上一次的残留值。
  useEffect(() => {
    if (open) {
      setValue(initialValue);
      setTouched(false);
    }
  }, [open, initialValue]);

  const trimmed = value.trim();
  const error = touched && validate ? validate(trimmed) : null;
  const canConfirm = trimmed.length > 0 && !(validate ? validate(trimmed) : null);

  const submit = () => {
    if (!canConfirm) {
      setTouched(true);
      return;
    }
    onSubmit(trimmed);
  };

  return (
    <Modal
      open={open}
      onClose={onCancel}
      width="base"
      ariaLabel={typeof title === "string" ? title : label}
      bodyClassName="p-0 overflow-hidden"
      footer={
        <>
          <Button variant="secondary" onClick={onCancel}>
            {cancelLabel}
          </Button>
          <Button variant="primary" onClick={submit} disabled={!canConfirm}>
            {confirmLabel}
          </Button>
        </>
      }
    >
      <div className="flex items-start gap-3.5 px-5 pt-5 pb-4">
        {icon && (
          <span
            aria-hidden
            className={cx(
              "grid size-9 shrink-0 place-items-center rounded-[var(--radius-md)]",
              "bg-accent-soft text-accent",
            )}
          >
            {icon}
          </span>
        )}
        <div className="min-w-0 flex-1">
          <h2 className="text-[length:var(--text-title)] font-semibold leading-snug text-[var(--color-text-primary)]">
            {title}
          </h2>
          <div className="mt-2.5">
            <Input
              autoFocus
              data-modal-autofocus
              label={label}
              labelHidden
              value={value}
              placeholder={placeholder}
              error={error ?? undefined}
              onChange={(e) => {
                setValue(e.target.value);
                if (!touched) setTouched(true);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter") submit();
                if (e.key === "Escape") onCancel();
              }}
            />
          </div>
        </div>
      </div>
    </Modal>
  );
}
