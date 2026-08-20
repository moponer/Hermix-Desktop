// 编辑本地用户档案弹窗。头像支持上传 PNG/JPG 并自动裁方 256×256；
// 也可清除回退到首字母。整窗 100% 走主题 token，无硬编码颜色。

import { useRef, useState, type CSSProperties, type ChangeEvent } from "react";
import { createPortal } from "react-dom";
import { UserAvatar } from "./UserAvatar";
import type { UserProfile } from "../hooks/useUserProfile";

interface EditProfileDialogProps {
  current: UserProfile;
  onSave: (next: Partial<UserProfile>) => void;
  onClose: () => void;
}

const TARGET = 256;
const MAX_BYTES = 2 * 1024 * 1024;

function fileToResizedDataUrl(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    if (file.size > MAX_BYTES) return reject(new Error("图片超过 2MB 限制"));
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("读取文件失败"));
    reader.onload = () => {
      const img = new Image();
      img.onerror = () => reject(new Error("图片解析失败"));
      img.onload = () => {
        const canvas = document.createElement("canvas");
        canvas.width = TARGET;
        canvas.height = TARGET;
        const ctx = canvas.getContext("2d");
        if (!ctx) return reject(new Error("canvas 不可用"));
        const scale = Math.max(TARGET / img.width, TARGET / img.height);
        const w = img.width * scale;
        const h = img.height * scale;
        ctx.drawImage(img, (TARGET - w) / 2, (TARGET - h) / 2, w, h);
        resolve(canvas.toDataURL("image/png"));
      };
      img.src = reader.result as string;
    };
    reader.readAsDataURL(file);
  });
}

export function EditProfileDialog({ current, onSave, onClose }: EditProfileDialogProps) {
  const [name, setName] = useState(current.displayName);
  const [handle, setHandle] = useState(current.handle);
  const [avatar, setAvatar] = useState<string | null>(current.avatarDataUrl);
  const [error, setError] = useState<string | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);

  const onPick = async (e: ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;
    try {
      setError(null);
      const url = await fileToResizedDataUrl(file);
      setAvatar(url);
    } catch (err) {
      setError(err instanceof Error ? err.message : "上传失败");
    } finally {
      e.target.value = "";
    }
  };

  const save = () => {
    onSave({
      displayName: name.trim() || "Hermix",
      handle: handle.trim() || "hermix",
      avatarDataUrl: avatar,
    });
  };

  return createPortal(
    <div
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      style={{
        position: "fixed",
        inset: 0,
        zIndex: 50,
        background: "rgba(0,0,0,0.55)",
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: 24,
      }}
    >
      <div
        role="dialog"
        aria-modal="true"
        aria-label="编辑我的资料"
        style={{
          width: "100%",
          maxWidth: 420,
          background: "var(--color-bg-elevated)",
          border: "1px solid var(--color-border-primary)",
          borderRadius: "var(--radius-lg)",
          padding: "20px 22px",
          color: "var(--color-text-primary)",
        }}
      >
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 16 }}>
          <h3 style={{ margin: 0, fontSize: 15, fontWeight: 500 }}>编辑我的资料</h3>
          <button onClick={onClose} aria-label="关闭" style={iconBtn}>
            ×
          </button>
        </div>

        <div style={{ display: "flex", gap: 18, alignItems: "flex-start", marginBottom: 18 }}>
          <UserAvatar name={name || "Hermix"} avatarDataUrl={avatar} size={72} />
          <div style={{ flex: 1, display: "flex", flexDirection: "column", gap: 6 }}>
            <button onClick={() => fileRef.current?.click()} style={primaryBtn}>
              上传头像
            </button>
            <button
              onClick={() => {
                setAvatar(null);
                setError(null);
              }}
              style={ghostBtn}
            >
              用字母自动生成
            </button>
            <p style={{ margin: "4px 0 0", fontSize: 11, color: "var(--color-text-tertiary)", lineHeight: 1.5 }}>
              支持 PNG / JPG，≤2MB，自动裁剪为正方形
            </p>
            <input
              ref={fileRef}
              type="file"
              accept="image/png,image/jpeg"
              onChange={onPick}
              style={{ display: "none" }}
            />
          </div>
        </div>

        <div style={field}>
          <label style={label} htmlFor="ep-name">
            显示名称
          </label>
          <input
            id="ep-name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            style={input}
          />
          <span style={hint}>聊天标题里看到的称呼。留空则用 "Hermix"。</span>
        </div>

        <div style={field}>
          <label style={label} htmlFor="ep-handle">
            本地标识{" "}
            <span style={{ color: "var(--color-text-tertiary)", fontWeight: 400 }}>
              （仅用于数据隔离目录名，不可外发）
            </span>
          </label>
          <input
            id="ep-handle"
            value={handle}
            onChange={(e) => setHandle(e.target.value)}
            style={input}
          />
        </div>

        {error && (
          <p style={{ margin: "0 0 12px", fontSize: 12, color: "var(--color-text-warning)" }}>{error}</p>
        )}

        <div
          style={{
            display: "flex",
            justifyContent: "flex-end",
            gap: 8,
            marginTop: 18,
            paddingTop: 14,
            borderTop: "1px solid var(--color-border-primary)",
          }}
        >
          <button onClick={onClose} style={ghostBtn}>
            取消
          </button>
          <button onClick={save} style={primaryBtn}>
            保存
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}

const iconBtn: CSSProperties = {
  width: 24,
  height: 24,
  border: 0,
  background: "transparent",
  cursor: "pointer",
  borderRadius: "var(--radius-md)",
  color: "var(--color-text-tertiary)",
  fontSize: 18,
  lineHeight: 1,
};
const primaryBtn: CSSProperties = {
  height: 32,
  padding: "0 14px",
  borderRadius: "var(--radius-md)",
  fontSize: 13,
  cursor: "pointer",
  background: "var(--color-bg-tertiary)",
  color: "var(--color-text-primary)",
  border: "1px solid var(--color-border-secondary)",
};
const ghostBtn: CSSProperties = {
  height: 32,
  padding: "0 14px",
  borderRadius: "var(--radius-md)",
  fontSize: 13,
  cursor: "pointer",
  background: "transparent",
  color: "var(--color-text-primary)",
  border: "1px solid var(--color-border-primary)",
};
const field: CSSProperties = { display: "flex", flexDirection: "column", gap: 6, marginBottom: 14 };
const label: CSSProperties = { fontSize: 13, color: "var(--color-text-secondary)", fontWeight: 500 };
const input: CSSProperties = {
  height: 36,
  padding: "0 10px",
  borderRadius: "var(--radius-md)",
  fontSize: 13,
  background: "var(--color-bg-primary)",
  color: "var(--color-text-primary)",
  border: "1px solid var(--color-border-secondary)",
  fontFamily: "inherit",
};
const hint: CSSProperties = { fontSize: 11, color: "var(--color-text-tertiary)" };
