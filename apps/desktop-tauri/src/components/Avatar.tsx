/// Avatars for user, AI assistant, and system.
/// - User / System: small geometric SVG marks that recolor with the theme
///   and stay legible at 18–40px. Pure black/white high-contrast.
/// - Assistant: the Hermix fox portrait (SVG). Vector source means @1x/@2x/
///   @3x all stay crisp without per-density exports; also removes the
///   `mascot-avatar-*.png` quartet from `src/assets/`.

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
      <rect x="0" y="0" width="40" height="40" rx="10" fill="var(--avatar-user-bg, #09090b)" />
      <circle cx="20" cy="16" r="6" fill="var(--avatar-user-mark, #fafafa)" opacity="0.95" />
      <path
        d="M 8 33 Q 8 24 20 24 Q 32 24 32 33 L 32 36 Q 32 38 30 38 L 10 38 Q 8 38 8 36 Z"
        fill="var(--avatar-user-mark, #fafafa)"
        opacity="0.95"
      />
    </svg>
  );
}

function AssistantAvatar({ size, className }: { size: number; className: string }) {
  // Vite asset URL — the `?url` suffix tells Vite to return the
  // raw file URL (not a base64 inline) for files in `public/`.
  // Using a hardcoded "/mascot-avatar-bg-64.png" string works in
  // a plain browser but can fail in Tauri where the custom asset
  // protocol origin (https://tauri.localhost) does not map the
  // root path the same way.
  return (
    <img
      src={new URL("/mascot-avatar-bg-64.png", import.meta.url).href}
      width={size}
      height={size}
      alt="Hermix 助手"
      draggable={false}
      // Avatar with slate-700 background — for chat bubble & desktop icon
      // 128px (any future hero card). No need for per-density PNG exports.
      className={`block object-contain select-none ${className}`}
      style={{
        // Pure circle — the user wants the AI avatar to read as a
        // round dot, not a rounded square, so it visually matches
        // the icon-sized avatar of typical chat UIs (Discord, iMessage).
        borderRadius: "50%",
        // Subtle ring for visual definition against both light and
        // dark backgrounds; the avatar is otherwise hard to spot
        // when it sits in a card of the same tone.
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
