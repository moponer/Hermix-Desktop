// 通用用户头像：有图显示图（圆形裁切），无图显示首字母（中性主题色）。
// 不依赖任何未定义 token，全部走 --color-bg-* / --color-text-* 主题变量。

import type { CSSProperties } from "react";

interface UserAvatarProps {
  name: string;
  avatarDataUrl?: string | null;
  size?: number;
  className?: string;
}

export function UserAvatar({ name, avatarDataUrl, size = 30, className = "" }: UserAvatarProps) {
  const initial = (name.trim()[0] || "U").toUpperCase();
  const box: CSSProperties = {
    width: size,
    height: size,
    borderRadius: "var(--radius-full)",
    flexShrink: 0,
    overflow: "hidden",
    display: "flex",
    alignItems: "center",
    justifyContent: "center",
  };

  if (avatarDataUrl) {
    return (
      <img
        src={avatarDataUrl}
        width={size}
        height={size}
        alt=""
        draggable={false}
        className={className}
        style={{ ...box, objectFit: "cover" }}
      />
    );
  }

  return (
    <div
      className={className}
      style={{
        ...box,
        background: "var(--color-bg-tertiary)",
        color: "var(--color-text-secondary)",
        fontSize: Math.max(11, Math.round(size * 0.42)),
        fontWeight: 500,
        boxShadow: "inset 0 0 0 0.5px var(--color-border-primary)",
      }}
      aria-label={name}
    >
      {initial}
    </div>
  );
}
