import { useCallback, useEffect, useState } from "react";

import { sendNotification } from "../desktopBridge";
import { storage, STORAGE_KEYS } from "../utils/storageCompat";

const CHANGED_EVT = "hermix:notify-pref-changed";

function read(): boolean {
  try {
    return storage.getItem(STORAGE_KEYS.notifyOnComplete) === "1";
  } catch {
    return false;
  }
}

/**
 * 桌面通知（L4）。
 *
 * 开关默认**关**，且只在"一轮回复结束 + 窗口当前没有焦点"时才弹。
 * 两个条件缺一不可：
 * · 没有焦点条件 → 用户正盯着屏幕，通知纯属噪声，而且会遮住输入区；
 * · 没有开关条件 → 用户无法表达"我开会时不想被打扰"。
 *
 * 失败一律吞掉并记 console：通知是锦上添花，它失败绝不能影响主流程，
 * 但也不能静默无痕——否则用户开了开关却什么都没收到，无从判断是条件
 * 没满足还是功能坏了。
 */
export function useDesktopNotify(): {
  enabled: boolean;
  setEnabled: (v: boolean) => void;
  notify: (title: string, body: string, force?: boolean) => void;
} {
  const [enabled, setEnabledState] = useState<boolean>(read);

  useEffect(() => {
    const onChange = () => setEnabledState(read());
    window.addEventListener(CHANGED_EVT, onChange);
    window.addEventListener("storage", onChange);
    return () => {
      window.removeEventListener(CHANGED_EVT, onChange);
      window.removeEventListener("storage", onChange);
    };
  }, []);

  const setEnabled = useCallback((v: boolean) => {
    try {
      if (v) storage.setItem(STORAGE_KEYS.notifyOnComplete, "1");
      else storage.removeItem(STORAGE_KEYS.notifyOnComplete);
    } catch {}
    setEnabledState(v);
    window.dispatchEvent(new CustomEvent(CHANGED_EVT));
  }, []);

  const notify = useCallback(
    (title: string, body: string, force = false) => {
      if (!force && (!enabled || document.hasFocus())) return;
      sendNotification(title, body).catch((e) => {
        console.error("[notify] 发送系统通知失败:", e);
      });
    },
    [enabled],
  );

  return { enabled, setEnabled, notify };
}
