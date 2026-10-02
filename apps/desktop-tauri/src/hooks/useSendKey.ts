import { useCallback, useEffect, useState } from "react";

import { storage, STORAGE_KEYS } from "../utils/storageCompat";

/** Enter 发送（Shift+Enter 换行）/ Ctrl+Enter 发送（Enter 换行）。 */
export type SendKey = "enter" | "ctrl_enter";

const CHANGED_EVT = "hermix:send-key-changed";

function read(): SendKey {
  try {
    if (storage.getItem(STORAGE_KEYS.sendKey) === "ctrl_enter") return "ctrl_enter";
  } catch {}
  return "enter";
}

/**
 * 发送快捷键偏好（L3.8）。
 *
 * 单一真相源 = localStorage。设置页与 ChatView 各持一份 React state，用
 * 自定义事件 + storage 事件互相同步——否则用户在设置页改完就切回输入框，
 * 拿到的是另一个组件里的旧值，且没有任何报错提示。
 */
export function useSendKey(): [SendKey, (v: SendKey) => void] {
  const [value, setValue] = useState<SendKey>(read);

  useEffect(() => {
    const onChange = () => setValue(read());
    window.addEventListener(CHANGED_EVT, onChange);
    window.addEventListener("storage", onChange);
    return () => {
      window.removeEventListener(CHANGED_EVT, onChange);
      window.removeEventListener("storage", onChange);
    };
  }, []);

  const set = useCallback((next: SendKey) => {
    try { storage.setItem(STORAGE_KEYS.sendKey, next); } catch {}
    setValue(next);
    window.dispatchEvent(new CustomEvent(CHANGED_EVT));
  }, []);

  return [value, set];
}
