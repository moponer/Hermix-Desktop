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
import * as api from "../../api";
import { toastError } from "../../hooks/useToast";

interface FormViewProps {
  module: Extract<WorkspaceModule, { type: "table" }>;
  initialValues?: Record<string, unknown>;
  /** 允许异步；失败时应由调用方 toast，本组件保持表单开启不丢输入。 */
  onSubmit: (values: Record<string, unknown>) => void | Promise<void>;
  onCancel?: () => void;
  submitLabel?: string;
  /** Relation 字段候选：目标表行清单（缺省退化为文本输入）。 */
  relationCandidates?: Record<string, { id: string; label: string }[]>;
  /** 工作台 id（P5 附件上传需要）。 */
  workspaceId?: string;
}

type FormState = Record<string, string | boolean>;

export function FormView({
  module,
  initialValues,
  onSubmit,
  onCancel,
  submitLabel = "保存",
  relationCandidates,
  workspaceId,
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
  const [busy, setBusy] = useState(false);

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
    // 异步提交：失败时上层 toast 且表单保持开启（不静默吞结果）
    setBusy(true);
    Promise.resolve(onSubmit(out)).finally(() => setBusy(false));
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
      <div className="flex shrink-0 items-center gap-2 px-3 py-1.5 text-[12px]" style={{ color: "var(--color-text-secondary)", borderBottom: "1px solid var(--color-border-subtle)" }}>
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
              candidates={relationCandidates?.[f.target ?? ""]}
              workspaceId={workspaceId}
              onChange={(v) => setField(f.id, v)}
            />
          ))}
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-2 border-t px-4 py-2.5" style={{ borderColor: "var(--color-border-subtle)" }}>
        <button
          onClick={handleSubmit}
          disabled={busy}
          className="rounded-md px-4 py-1.5 text-[12px] font-medium transition-opacity hover:opacity-85 disabled:opacity-50"
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
  candidates,
  workspaceId,
  onChange,
}: {
  field: WorkspaceField;
  value: string | boolean;
  error?: string;
  /** Relation 候选（field.target 表的行清单）。 */
  candidates?: { id: string; label: string }[];
  /** 工作台 id（P5 附件上传需要）。 */
  workspaceId?: string;
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
          className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
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
          className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
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
          className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
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
          className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
          style={inputStyle}
        />
      );
      break;
    case "relation": {
      // 有候选清单（父层已缓存目标表行）→ 下拉选择；否则退化为文本输入。
      // 不要求用户手输行 UUID——那等价于没有这个字段。
      if (candidates && candidates.length > 0) {
        control = (
          <select
            id={`ws-f-${field.id}`}
            value={typeof value === "string" ? value : ""}
            onChange={(e) => onChange(e.target.value)}
            className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
            style={inputStyle}
          >
            <option value="">（不关联）</option>
            {candidates.map((c) => (
              <option key={c.id} value={c.id}>{c.label}</option>
            ))}
          </select>
        );
      } else {
        control = (
          <input
            id={`ws-f-${field.id}`}
            type="text"
            value={typeof value === "string" ? value : ""}
            onChange={(e) => onChange(e.target.value)}
            placeholder={`关联 ${field.target ?? "?"} 表行 id`}
            className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
            style={inputStyle}
          />
        );
      }
      break;
    }
    case "attachment": {
      // P5：选择文件 → FileReader base64 → 上传 → 值 = 相对路径（files/<name>）
      const current = typeof value === "string" ? value : "";
      control = (
        <div className="flex flex-col gap-1">
          <input
            id={`ws-f-${field.id}`}
            type="file"
            className="rounded-md border px-2.5 py-1.5 text-[12px]"
            style={inputStyle}
            onChange={(e) => {
              const input = e.currentTarget;
              const f = input.files?.[0];
              if (!f) return;
              if (!workspaceId) {
                toastError("工作台未就绪，无法上传附件");
                input.value = "";
                return;
              }
              const reader = new FileReader();
              reader.onload = () => {
                const b64 = String(reader.result).split(",")[1] ?? "";
                void (async () => {
                  try {
                    const res = await api.uploadWorkspaceFile(workspaceId, f.name, b64);
                    onChange(res.path);
                  } catch (err) {
                    toastError(`上传附件失败: ${String(err)}`);
                  } finally {
                    // 清空 input 以便重选同一文件也能再次触发 onChange
                    input.value = "";
                  }
                })();
              };
              reader.readAsDataURL(f);
            }}
          />
          {current && (
            <span className="truncate text-[11px]" style={{ color: "var(--color-text-secondary)" }}>
              已选: {current}
            </span>
          )}
        </div>
      );
      break;
    }
    default: // text
      control = (
        <input
          id={`ws-f-${field.id}`}
          type="text"
          value={typeof value === "string" ? value : ""}
          onChange={(e) => onChange(e.target.value)}
          className="w-full rounded-md border px-2.5 py-1.5 text-[13px] outline-none"
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
