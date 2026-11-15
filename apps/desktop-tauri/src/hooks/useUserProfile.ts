// 本地用户档案（无登录体系，纯本机可编辑）。
// 持久化到 localStorage（经 storageCompat 兼容 hermes→hermix 重命名），
// 不上传任何后端。改名称只影响 UI 显示，不影响已存在的对话历史与缓存路径。

import { useCallback, useState } from "react";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

export interface UserProfile {
  /** 聊天标题里看到的称呼，默认 "Hermix"。 */
  displayName: string;
  /** 上传头像的 base64 data URL（PNG，已裁方 256×256），无则 null 回退首字母。 */
  avatarDataUrl: string | null;
  /** 仅用于本机数据隔离目录名，不可外发。 */
  handle: string;
}

const DEFAULT_PROFILE: UserProfile = {
  displayName: "Hermix",
  avatarDataUrl: null,
  handle: "hermix",
};

function load(): UserProfile {
  try {
    const raw = storage.getItem(STORAGE_KEYS.userProfile);
    if (!raw) return DEFAULT_PROFILE;
    const parsed = JSON.parse(raw) as Partial<UserProfile>;
    return { ...DEFAULT_PROFILE, ...parsed };
  } catch {
    return DEFAULT_PROFILE;
  }
}

export function useUserProfile() {
  const [profile, setProfileState] = useState<UserProfile>(load);

  const setProfile = useCallback((next: Partial<UserProfile>) => {
    setProfileState((prev) => {
      const merged: UserProfile = { ...prev, ...next };
      storage.setItem(STORAGE_KEYS.userProfile, JSON.stringify(merged));
      return merged;
    });
  }, []);

  return { profile, setProfile };
}
