import { forwardRef, useId, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from "react";

import { cx } from "../../lib/cx";

/**
 * 控件高度只有三档（24 / 32 / 40）。此前是 sm/md/lg/xl 四档（28/32/40/44），
 * 加上各页面手写的 px-3 py-2（≈37）、px-2.5 py-1.5（≈30）、.modal-input（≈36），
 * 同屏能数出 6 种高度。三档对齐宪法 §五的控件高度档。
 */
export type FieldSize = "sm" | "md" | "lg";

/**
 * 字段的基类，`Input` / `TextArea` / `SelectField` 共用，避免三处分叉。
 *
 * **无描边**（用户明确否过「输入框描边」）：字段的可辨识度由**底色差**承担
 * —— 控件是 `--color-bg-tertiary` 的一档，比卡片面（`--color-bg-card`）深一档，
 * 与页面纸面（`--color-bg-primary`）差两档。不画线，不用边框假装层次。
 * 唯一的环是 `focus-visible` 的 accent 环（无障碍必需）与错误态的淡红底。
 */
export const FIELD_BASE_CLASSES = [
  "w-full rounded-[var(--radius-md)] outline-none",
  "text-[var(--color-text-primary)]",
  "placeholder:text-[var(--color-text-tertiary)]",
  "transition-colors duration-150",
  /* 焦点态全 app 唯一形态：2px accent 环 + 2px 偏移（宪法 §七.2）。
     偏移用透明色 —— 缝隙透出下方表面色，控件坐在任何底色上都不出现接缝
     （写死 --color-bg-card 会在白底页面上留一道浅灰边）。 */
  "focus-visible:ring-2 focus-visible:ring-[var(--color-accent)] focus-visible:ring-offset-2 focus-visible:ring-offset-transparent",
  "disabled:cursor-not-allowed disabled:opacity-60 disabled:bg-[var(--color-bg-quaternary)]",
].join(" ");

/**
 * 字段底色。**有且只有一个 bg 类** —— 正常态与错误态都从这里出，
 * 否则两个任意值 bg 工具类谁生效取决于 Tailwind 生成顺序，不可预测。
 * 错误态用**淡红底**而不是红描边（描边一律不要）。
 */
export const fieldStateClasses = (invalid?: boolean) =>
  invalid ? "bg-[var(--color-error-soft)]" : "bg-[var(--color-bg-tertiary)]";

const FIELD_SIZE: Record<FieldSize, string> = {
  sm: "h-6 px-2 text-[length:var(--text-meta)]",
  md: "h-8 px-2.5 text-xs",
  lg: "h-10 px-3 text-[length:var(--text-ui)]",
};

function FieldShell({
  id,
  label,
  labelHidden,
  hint,
  hintId,
  error,
  errorId,
  required,
  rightAction,
  containerClassName,
  children,
}: {
  id: string;
  label?: ReactNode;
  /** 只有无障碍名、没有可见文字时用它。 */
  labelHidden?: boolean;
  hint?: ReactNode;
  hintId?: string;
  error?: ReactNode;
  errorId?: string;
  required?: boolean;
  /** label 右侧的附加动作（如「AI 生成」按钮）。 */
  rightAction?: ReactNode;
  containerClassName?: string;
  children: ReactNode;
}) {
  return (
    <div className={cx("flex flex-col gap-1", containerClassName)}>
      {(label || rightAction) && (
        <div className="flex items-center justify-between gap-2">
          {label && (
            <label htmlFor={id} className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
              {labelHidden ? <span className="sr-only">{label}</span> : label}
              {required && (
                <span aria-hidden className="ml-0.5 text-[var(--color-error)]">
                  *
                </span>
              )}
            </label>
          )}
          {rightAction}
        </div>
      )}
      {children}
      {hint && !error && (
        <p id={hintId} className="text-[length:var(--text-meta)] text-[var(--color-text-tertiary)]">
          {hint}
        </p>
      )}
      {error && (
        <p id={errorId} role="alert" className="text-[length:var(--text-meta)] text-[var(--color-error)]">
          {error}
        </p>
      )}
    </div>
  );
}

/** 由字段组件算好三件套：控件 id / 提示 id / 错误 id。 */
function useFieldIds(id: string | undefined, hint: unknown, error: unknown) {
  const auto = useId();
  const fieldId = id ?? auto;
  // 提示与错误互斥（错误出现时提示让位），所以关联哪一个由 error 决定。
  const hintId = hint ? `${fieldId}-hint` : undefined;
  const errorId = error ? `${fieldId}-error` : undefined;
  const describedBy = error ? errorId : hint ? hintId : undefined;
  return { fieldId, hintId, errorId, describedBy };
}

export interface InputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "size"> {
  size?: FieldSize;
  label?: ReactNode;
  /** 只有 label 而没有可见文字时用它。 */
  labelHidden?: boolean;
  hint?: ReactNode;
  error?: ReactNode;
  /** label 右侧的附加动作（如「AI 生成」按钮）。 */
  rightAction?: ReactNode;
  containerClassName?: string;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { size = "lg", label, labelHidden, hint, error, required, rightAction, containerClassName, className, id, ...props },
  ref,
) {
  const { fieldId, hintId, errorId, describedBy } = useFieldIds(id, hint, error);
  return (
    <FieldShell
      id={fieldId}
      label={label}
      labelHidden={labelHidden}
      hint={hint}
      hintId={hintId}
      error={error}
      errorId={errorId}
      required={required}
      rightAction={rightAction}
      containerClassName={containerClassName}
    >
      <input
        ref={ref}
        id={fieldId}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy}
        className={cx(FIELD_BASE_CLASSES, FIELD_SIZE[size], fieldStateClasses(!!error), className)}
        {...props}
      />
    </FieldShell>
  );
});

export interface TextAreaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  label?: ReactNode;
  labelHidden?: boolean;
  hint?: ReactNode;
  error?: ReactNode;
  /** label 右侧的附加动作（如「AI 生成」按钮）。 */
  rightAction?: ReactNode;
  containerClassName?: string;
}

export const TextArea = forwardRef<HTMLTextAreaElement, TextAreaProps>(function TextArea(
  { label, labelHidden, hint, error, required, rightAction, containerClassName, className, id, rows = 3, ...props },
  ref,
) {
  const { fieldId, hintId, errorId, describedBy } = useFieldIds(id, hint, error);
  return (
    <FieldShell
      id={fieldId}
      label={label}
      labelHidden={labelHidden}
      hint={hint}
      hintId={hintId}
      error={error}
      errorId={errorId}
      required={required}
      rightAction={rightAction}
      containerClassName={containerClassName}
    >
      <textarea
        ref={ref}
        id={fieldId}
        rows={rows}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy}
        className={cx(
          FIELD_BASE_CLASSES,
          "px-3 py-2 text-[length:var(--text-ui)] resize-y",
          fieldStateClasses(!!error),
          className,
        )}
        {...props}
      />
    </FieldShell>
  );
});

export interface SelectOption {
  value: string;
  label: string;
}

export interface SelectFieldProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, "size"> {
  size?: FieldSize;
  label?: ReactNode;
  /** 只有 label 而没有可见文字时用它。 */
  labelHidden?: boolean;
  options: SelectOption[];
  hint?: ReactNode;
  error?: ReactNode;
  /** label 右侧的附加动作。 */
  rightAction?: ReactNode;
  containerClassName?: string;
}

/** 用原生 `<select>` 是有意的：免费拿到移动端系统选择器，
 *  自绘下拉在触屏上要重做一整套。 */
export const SelectField = forwardRef<HTMLSelectElement, SelectFieldProps>(function SelectField(
  { size = "lg", label, labelHidden, options, hint, error, required, rightAction, containerClassName, className, id, ...props },
  ref,
) {
  const { fieldId, hintId, errorId, describedBy } = useFieldIds(id, hint, error);
  return (
    <FieldShell
      id={fieldId}
      label={label}
      labelHidden={labelHidden}
      hint={hint}
      hintId={hintId}
      error={error}
      errorId={errorId}
      required={required}
      rightAction={rightAction}
      containerClassName={containerClassName}
    >
      <select
        ref={ref}
        id={fieldId}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={describedBy}
        className={cx(
          FIELD_BASE_CLASSES,
          FIELD_SIZE[size],
          "cursor-pointer",
          fieldStateClasses(!!error),
          className,
        )}
        {...props}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </FieldShell>
  );
});
