/// Avatars for user, AI assistant, and system.
/// - User / System: small geometric SVG marks that recolor with the theme
/// and stay legible at 18–40px. Pure black/white high-contrast.
/// - Assistant: the Hermix fox portrait (SVG). Vector source means @1x/@2x/
/// @3x all stay crisp without per-density exports; also removes the
/// `mascot-avatar-*.png` quartet from `src/assets/`.

import { useState } from "react";

interface AvatarProps {
  role: "user" | "assistant" | "system";
  size?: number;
  className?: string;
}

export function Avatar({ role, size = 35, className = "" }: AvatarProps) {
  if (role === "user") return <UserAvatar size={size} className={className} />;
  if (role === "system") return <SystemAvatar size={size} className={className} />;
  return <AssistantAvatar size={size} className={className} />;
}

function UserAvatar({ size, className }: { size: number; className: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 40 40"
      className={className}
      role="img"
      aria-label="用户头像"
    >
      <rect x="0" y="0" width="40" height="40" rx="10" fill="var(--avatar-user-bg)" />
      <circle cx="20" cy="16" r="6" fill="var(--avatar-user-mark)" opacity="0.95" />
      <path
        d="M 8 33 Q 8 24 20 24 Q 32 24 32 33 L 32 36 Q 32 38 30 38 L 10 38 Q 8 38 8 36 Z"
        fill="var(--avatar-user-mark)"
        opacity="0.95"
      />
    </svg>
  );
}

function AssistantAvatar({ size, className }: { size: number; className: string }) {
  // Files in `public/` should be referenced by root-relative string.
  // `new URL("/...", import.meta.url)` can mis-resolve under Tauri's
  // custom asset protocol and leave a broken-image alt text squeezed
  // into the 22px circle.
  const [failed, setFailed] = useState(false);
  if (failed) {
    return (
      <div
        className={`inline-flex items-center justify-center ${className}`}
        style={{
          width: size,
          height: size,
          borderRadius: "50%",
          background: "var(--color-bg-secondary)",
          boxShadow: "0 0 0 1px var(--color-border-subtle)",
          color: "var(--color-text-muted)",
          fontSize: Math.max(10, size * 0.42),
          fontWeight: 600,
          flexShrink: 0,
        }}
        aria-label="Hermix 助手"
      >
        AI
      </div>
    );
  }
  return (
    <img
      src="/mascot-avatar-bg-64.png"
      width={size}
      height={size}
      alt=""
      draggable={false}
      onError={() => setFailed(true)}
      className={`block object-contain select-none ${className}`}
      style={{
        borderRadius: "50%",
        background: "var(--color-bg-secondary)",
        boxShadow: "0 0 0 1px var(--color-border-subtle)",
      }}
    />
  );
}

function SystemAvatar({ size, className }: { size: number; className: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 40 40"
      className={className}
      role="img"
      aria-label="系统"
    >
      <rect x="0" y="0" width="40" height="40" rx="10" fill="var(--color-bg-tertiary)" />
      <rect x="10" y="14" width="20" height="12" rx="2" fill="none" stroke="var(--color-text-muted)" strokeWidth="1.5" />
      <line x1="14" y1="18" x2="26" y2="18" stroke="var(--color-text-muted)" strokeWidth="1.5" />
      <line x1="14" y1="22" x2="22" y2="22" stroke="var(--color-text-muted)" strokeWidth="1.5" />
    </svg>
  );
}
