/**
 * FormView — 工作台表单视图（M1.5）。
 *
 * 按 schema 字段类型渲染输入控件：
 * text→input / number→input[number] / date→input[date] /
 * datetime→input[datetime-local] / boolean→checkbox / enum→select /
 * relation→text（关联目标 id，M3 工具族接入后由查询填充）/ attachment→file。
 *
 * 必填校验在提交前执行；数值/布尔在提交时转换类型。
 */

import { useMemo, useState } from "react";
import type { WorkspaceField, WorkspaceModule } from "../../types";

interface FormViewProps {
  module: Extract<WorkspaceModule, { type: "table" }>;
  initialValues?: Record<string, unknown>;
  onSubmit: (values: Record<string, unknown>) => void;
  onCancel?: () => void;
  submitLabel?: string;
}

type FormState = Record<string, string | boolean>;

export function FormView({
  module,
  initialValues,
  onSubmit,
  onCancel,
  submitLabel = "保存",
}: FormViewProps) {
  const [values, setValues] = useState<FormState>(() => {
    const init: FormState = {};
    for (const f of module.fields) {
      const v = initialValues?.[f.id];
      if (f.type === "boolean") {
        init[f.id] = typeof v === "boolean" ? v : false;
      } else if (v !== undefined && v !== null) {
        init[f.id] = String(v);
      } else if (f.type === "enum" && f.options?.length) {
        init[f.id] = f.options[0]; // 枚举默认第一项
      } else {
        init[f.id] = "";
      }
    }
    return init;
  });
  const [errors, setErrors] = useState<Record<string, string>>({});

  const setField = (id: string, v: string | boolean) =>
    setValues((prev) => ({ ...prev, [id]: v }));

  const handleSubmit = () => {
    const errs: Record<string, string> = {};
    for (const f of module.fields) {
      const v = values[f.id];
      if (f.required && (typeof v !== "boolean" && String(v).trim() === "")) {
        errs[f.id] = `${f.name} 必填`;
      }
    }
    setErrors(errs);
    if (Object.keys(errs).length > 0) return;

    const out: Record<string, unknown> = {};
    for (const f of module.fields) {
      const v = values[f.id];
      if (f.type === "boolean") {
        out[f.id] = v === true;
      } else if (f.type === "number") {
        out[f.id] = v === "" ? null : Number(v);
      } else {
        out[f.id] = v === "" ? null : v;
      }
    }
    onSubmit(out);
  };

  const fields = useMemo(() => module.fields, [module.fields]);
  if (fields.length === 0) {
    return (
      <div className="flex flex-1 items-center justify-center text-[12px]" style={{ color: "var(--color-text-faint)" }}>
        该表还没有字段
      </div>
    );
  }

  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="flex shrink-0 items-center gap-2 px-3 py-1.5 text-[11.5px]" style={{ color: "var(--color-text-secondary)", borderBottom: "1px solid var(--color-border-subtle)" }}>
        <span className="font-medium" style={{ color: "var(--color-text-primary)" }}>{module.name} — 表单</span>
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-4 py-3">
        <div className="mx-auto flex max-w-[560px] flex-col gap-3">
          {fields.map((f) => (
            <FieldInput
              key={f.id}
              field={f}
              value={values[f.id]}
              error={errors[f.id]}
              onChange={(v) => setField(f.id, v)}
            />
          ))}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-2 border-t px-4 py-2.5" style={{ borderColor: "var(--color-border-subtle)" }}>
        <button
          onClick={handleSubmit}
          className="rounded-md px-4 py-1.5 text-[12px] font-medium transition-opacity hover:opacity-85"
          style={{ background: "var(--color-accent)", color: "var(--color-accent-on-text)" }}
        >
          {submitLabel}
        </button>
        {onCancel && (
          <button
            onClick={onCancel}
            className="rounded-md px-3 py-1.5 text-[12px]"
            style={{ color: "var(--color-text-secondary)", background: "var(--color-bg-tertiary)" }}
          >
            取消
          </button>
        )}
      </div>
    </div>
  );
}

function FieldInput({
  field,
  value,
  error,
  onChange,
}: {
  field: WorkspaceField;
  value: string | boolean;
  error?: string;
  onChange: (v: string | boolean) => void;
}) {
  const label = (
    <label
      htmlFor={`ws-f-${field.id}`}
      className="mb-1 block text-[12px] font-medium"
      style={{ color: "var(--color-text-secondary)" }}
    >
      {field.name}
      {field.required && <span style={{ color: "var(--color-error)" }}> *</span>}
    </label>
  );
  const inputStyle = {
    borderColor: error ? "var(--color-error)" : "var(--color-border-secondary)",
    background: "var(--color-bg-input, var(--color-bg-base))",
    color: "var(--color-text-primary)",
  } as const;

  let control: React.ReactNode;
  switch (field.type) {
    case "boolean":
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="checkbox"
          checked={value === true}
          onChange={(e) => onChange(e.target.checked)}
          className="h-4 w-4 accent-[var(--color-accent)]"
        />
      );
      break;
    case "enum":
      control = (
        <select
          id={`ws-f-${field.id}`}
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-md border px-2.5 py-1.5 text-[12.5px] outline-none"
          style={inputStyle}
        >
          {(field.options ?? []).map((o) => (
            <option key={o} value={o}>{o}</option>
          ))}
        </select>
      );
      break;
    case "number":
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="number"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-md border px-2.5 py-1.5 text-[12.5px] outline-none"
          style={inputStyle}
        />
      );
      break;
    case "date":
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="date"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-md border px-2.5 py-1.5 text-[12.5px] outline-none"
          style={inputStyle}
        />
      );
      break;
    case "datetime":
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="datetime-local"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-md border px-2.5 py-1.5 text-[12.5px] outline-none"
          style={inputStyle}
        />
      );
      break;
    case "relation":
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="text"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          placeholder={`关联 ${field.target ?? "?"} 表行 id`}
          className="w-full rounded-md border px-2.5 py-1.5 text-[12.5px] outline-none"
          style={inputStyle}
        />
      );
      break;
    case "attachment":
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="file"
          className="w-full rounded-md border px-2.5 py-1.5 text-[12px] outline-none"
          style={inputStyle}
        />
      );
      break;
    default: // text
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="text"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-md border px-2.5 py-1.5 text-[12.5px] outline-none"
          style={inputStyle}
        />
      );
  }

  return (
    <div>
      {label}
      {control}
      {error && <div className="mt-0.5 text-[11px]" style={{ color: "var(--color-error)" }}>{error}</div>}
    </div>
  );
}
