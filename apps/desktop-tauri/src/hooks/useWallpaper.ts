import { useCallback, useEffect, useState } from "react";

import {
  clearWallpaper as clearWallpaperCmd,
  getWallpaper as getWallpaperCmd,
  setWallpaper as setWallpaperCmd,
  type WallpaperInfo,
} from "../desktopBridge";

/**
 * 壁纸（L3.6）——**模块级共享 store**，不是每组件一份 state。
 *
 * 为什么必须是共享的：App 根和设置页都要用。若各挂一个实例，设置页改完
 * 壁纸后 App 根那份还持有旧 url，它后续任何一次重渲染都可能把 CSS 变量
 * 写回旧值——表现就是"设置页说换好了、主界面没变"，且时有时无。这是
 * 典型的双权威 bug，与本项目 ModelSelector 踩过的那个同源。
 *
 * 渲染用后端内联的 base64 data URL（`info.data_url`），**不依赖 Tauri asset
 * 协议作用域**：之前靠 `convertFileSrc` + `$HOME/.hermix/**` 作用域，在
 * Windows 上 `$HOME` 解析失配，资产 URL 加载失败，预览与生效双双空白。
 * data URL 内联进 CSS 变量，零作用域、零 CORS。
 *
 * 注入方式是给根元素挂 CSS 变量 + data 属性，由 `components-layout.css` 的
 * `[data-wallpaper="on"]` 系列规则消费，整窗铺底。不用 React 节点直接铺：
 * 壁纸要垫在标题栏、三栏、所有浮层**之下**，节点方案要跟每个 overlay 抢
 * z-index，迟早落到某个浮层之下看不见。
 */

interface Store {
  info: WallpaperInfo | null;
  url: string | null;
  loading: boolean;
}

const state: Store = { info: null, url: null, loading: true };
const listeners = new Set<(s: Store) => void>();

function emit() {
  // 必须传**副本**：`state` 是模块级同一个对象引用，原样传给 setState
  // 会被 React 的 Object.is 判定为"没变"而跳过重渲染，订阅者拿到旧值。
  const snap = { ...state };
  for (const l of listeners) l(snap);
}

/** 把当前状态落到 DOM。**所有**变更路径都必须经过它——两处各写一遍
 * setProperty 是这个功能唯一会出 bug 的地方。 */
function applyToDom() {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (state.url) {
    root.dataset.wallpaper = "on";
    // data URL 可能含特殊字符，但包进 url("…") 里是合法的（base64 只有
    // 安全字符）；为稳妥这里仍用双引号包裹。
    root.style.setProperty("--wallpaper-image", `url("${state.url}")`);
  } else {
    delete root.dataset.wallpaper;
    root.style.removeProperty("--wallpaper-image");
  }
}

function setStore(next: Partial<Store>) {
  Object.assign(state, next);
  applyToDom();
  emit();
}

let loaded = false;

async function loadOnce() {
  try {
    const info = await getWallpaperCmd();
    setStore({ info, url: info?.data_url ?? null });
  } catch (e) {
    console.error("[useWallpaper] 读取壁纸失败:", e);
    setStore({ info: null, url: null });
  } finally {
    loaded = true;
    setStore({ loading: false });
  }
}

export function useWallpaper() {
  const [snap, setSnap] = useState<Store>(state);

  useEffect(() => {
    listeners.add(setSnap);
    return () => {
      listeners.delete(setSnap);
    };
  }, []);

  useEffect(() => {
    if (loaded) {
      applyToDom();
      return;
    }
    void loadOnce();
  }, []);

  const setFrom = useCallback(async (sourcePath: string) => {
    const info = await setWallpaperCmd(sourcePath);
    setStore({ info, url: info.data_url });
  }, []);

  const clear = useCallback(async () => {
    await clearWallpaperCmd();
    setStore({ info: null, url: null });
  }, []);

  return {
    info: snap.info,
    url: snap.url,
    loading: snap.loading,
    setFrom,
    clear,
  };
}
