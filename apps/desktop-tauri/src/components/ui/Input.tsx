import { forwardRef, useId, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes, type TextareaHTMLAttributes } from "react";

import { cx } from "../../lib/cx";

export type FieldSize = "sm" | "md" | "lg" | "xl";

/** 字段的基类，`Input` / `TextArea` / `SelectField` 共用，避免三处分叉。 */
export const FIELD_BASE_CLASSES = [
  "w-full rounded-[var(--radius-md)] border outline-none",
  "bg-[var(--color-bg-card)] text-[var(--color-text-primary)]",
  "placeholder:text-[var(--color-text-tertiary)]",
  "transition-colors duration-150",
  "focus:border-[var(--color-accent)]",
  "disabled:cursor-not-allowed disabled:opacity-60 disabled:bg-[var(--color-bg-tertiary)]",
].join(" ");

/** 非法态。抽成函数是因为三个字段组件都要用同一套。 */
export const fieldStateClasses = (invalid?: boolean) =>
  invalid ? "border-[var(--color-error)]" : "border-[var(--color-border-secondary)]";

const FIELD_SIZE: Record<FieldSize, string> = {
  sm: "h-7 px-2 text-[length:var(--text-meta)]",
  md: "h-8 px-2.5 text-xs",
  lg: "h-10 px-3 text-[length:var(--text-ui)]",
  xl: "h-11 px-3.5 text-sm",
};

function FieldShell({
  id,
  label,
  hint,
  error,
  required,
  containerClassName,
  children,
}: {
  id: string;
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  required?: boolean;
  containerClassName?: string;
  children: ReactNode;
}) {
  return (
    <div className={cx("flex flex-col gap-1", containerClassName)}>
      {label && (
        <label htmlFor={id} className="text-[length:var(--text-ui)] font-medium text-[var(--color-text-primary)]">
          {label}
          {required && <span className="ml-0.5 text-[var(--color-error)]">*</span>}
        </label>
      )}
      {children}
      {hint && !error && <p className="text-[length:var(--text-meta)] text-[var(--color-text-tertiary)]">{hint}</p>}
      {error && (
        <p role="alert" className="text-[length:var(--text-meta)] text-[var(--color-error)]">
          {error}
        </p>
      )}
    </div>
  );
}

export interface InputProps extends Omit<InputHTMLAttributes<HTMLInputElement>, "size"> {
  size?: FieldSize;
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  containerClassName?: string;
}

export const Input = forwardRef<HTMLInputElement, InputProps>(function Input(
  { size = "lg", label, hint, error, required, containerClassName, className, id, ...props },
  ref,
) {
  const auto = useId();
  const fieldId = id ?? auto;
  return (
    <FieldShell
      id={fieldId}
      label={label}
      hint={hint}
      error={error}
      required={required}
      containerClassName={containerClassName}
    >
      <input
        ref={ref}
        id={fieldId}
        aria-invalid={error ? true : undefined}
        className={cx(FIELD_BASE_CLASSES, FIELD_SIZE[size], fieldStateClasses(!!error), className)}
        {...props}
      />
    </FieldShell>
  );
});

export interface TextAreaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  label?: ReactNode;
  hint?: ReactNode;
  error?: ReactNode;
  containerClassName?: string;
}

export const TextArea = forwardRef<HTMLTextAreaElement, TextAreaProps>(function TextArea(
  { label, hint, error, required, containerClassName, className, id, rows = 3, ...props },
  ref,
) {
  const auto = useId();
  const fieldId = id ?? auto;
  return (
    <FieldShell
      id={fieldId}
      label={label}
      hint={hint}
      error={error}
      required={required}
      containerClassName={containerClassName}
    >
      <textarea
        ref={ref}
        id={fieldId}
        rows={rows}
        aria-invalid={error ? true : undefined}
        className={cx(FIELD_BASE_CLASSES, "px-3 py-2 text-[length:var(--text-ui)] resize-y", fieldStateClasses(!!error), className)}
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
  containerClassName?: string;
}

/** 用原生 `<select>` 是有意的：免费拿到移动端系统选择器，
 *  自绘下拉在触屏上要重做一整套。 */
export const SelectField = forwardRef<HTMLSelectElement, SelectFieldProps>(function SelectField(
  { size = "lg", label, labelHidden, options, hint, error, required, containerClassName, className, id, ...props },
  ref,
) {
  const auto = useId();
  const fieldId = id ?? auto;
  return (
    <FieldShell
      id={fieldId}
      label={labelHidden ? <span className="sr-only">{label}</span> : label}
      hint={hint}
      error={error}
      required={required}
      containerClassName={containerClassName}
    >
      <select
        ref={ref}
        id={fieldId}
        aria-invalid={error ? true : undefined}
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
