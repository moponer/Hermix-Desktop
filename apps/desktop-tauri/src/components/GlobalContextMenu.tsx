/**
 * 全局右键菜单 —— 把浏览器的网页菜单换成应用菜单。
 *
 * 挂在应用根上一次，全窗口生效：
 *
 * · **主文档**：捕获阶段 `preventDefault()`，网页菜单永远不出现；再按
 *   右键目标算出该给什么项（输入框给剪切/复制/粘贴/全选，选中文字给复制，
 *   图片给复制图片，链接给复制地址/打开）。冒泡到 document 时若发现某个
 *   组件自己处理了这次右键（`markContextMenuHandled`），就让位给它。
 *
 * · **工作台 iframe**：跨源，宿主读不到它的 DOM —— 由工作台 SDK 拦截
 *   右键并 postMessage 过来，宿主用同一套菜单渲染（坐标按 iframe 的
 *   位置换算成视口坐标）。
 *
 * 菜单本体在 `ui/ContextMenu.tsx`（共享原语层，页面不另写一份）。
 */
import { useCallback, useEffect, useState } from "react";
import {
  ClipboardPaste,
  Copy,
  ExternalLink,
  Image as ImageIcon,
  Link2,
  Scissors,
  TextSelect,
} from "lucide-react";

import {
  CONTEXT_MENU_CLASS,
  ContextMenu,
  isContextMenuHandled,
  type ContextMenuItem,
} from "./ui";
import { toastError } from "../hooks/useToast";

interface MenuState {
  x: number;
  y: number;
  items: ContextMenuItem[];
}

/** 可编辑元素（输入框 / 文本域 / contenteditable）。 */
function editableOf(target: EventTarget | null): HTMLElement | null {
  if (!(target instanceof HTMLElement)) return null;
  const el = target.closest<HTMLElement>(
    "input, textarea, [contenteditable='true'], [contenteditable='']",
  );
  if (!el) return null;
  if (el instanceof HTMLInputElement) {
    // 按钮/复选框等不是文本编辑区，右键它们没有剪切粘贴的意义。
    const editableTypes = new Set([
      "text",
      "search",
      "url",
      "email",
      "tel",
      "password",
      "number",
    ]);
    if (!editableTypes.has(el.type)) return null;
  }
  return el;
}

/** 取当前选区文本（输入框取选中片段，正文取 document selection）。 */
function selectionText(editable: HTMLElement | null): string {
  if (editable instanceof HTMLInputElement || editable instanceof HTMLTextAreaElement) {
    return editable.value.slice(editable.selectionStart ?? 0, editable.selectionEnd ?? 0);
  }
  return window.getSelection()?.toString() ?? "";
}

/** 记下选区，菜单点击后还能把它放回去（菜单夺走焦点会丢选区）。 */
function captureSelection(editable: HTMLElement | null) {
  if (editable instanceof HTMLInputElement || editable instanceof HTMLTextAreaElement) {
    return {
      editable,
      start: editable.selectionStart ?? 0,
      end: editable.selectionEnd ?? 0,
      range: null as Range | null,
    };
  }
  const sel = window.getSelection();
  const range = sel && sel.rangeCount > 0 ? sel.getRangeAt(0) : null;
  return { editable, start: 0, end: 0, range };
}

function restoreSelection(cap: ReturnType<typeof captureSelection>) {
  if (!cap.editable) return;
  cap.editable.focus();
  if (cap.range) {
    const sel = window.getSelection();
    sel?.removeAllRanges();
    sel?.addRange(cap.range);
  } else if (cap.editable instanceof HTMLInputElement || cap.editable instanceof HTMLTextAreaElement) {
    cap.editable.setSelectionRange(cap.start, cap.end);
  }
}

async function writeClipboardText(text: string): Promise<void> {
  await navigator.clipboard.writeText(text);
}

async function copyImageToClipboard(img: HTMLImageElement): Promise<void> {
  const canvas = document.createElement("canvas");
  canvas.width = img.naturalWidth || img.width;
  canvas.height = img.naturalHeight || img.height;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("2d context unavailable");
  ctx.drawImage(img, 0, 0);
  const blob = await new Promise<Blob | null>((resolve) => canvas.toBlob(resolve, "image/png"));
  if (!blob) throw new Error("toBlob failed");
  if (typeof ClipboardItem === "undefined") throw new Error("ClipboardItem unsupported");
  await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
}

export function GlobalContextMenu() {
  const [menu, setMenu] = useState<MenuState | null>(null);

  const close = useCallback(() => setMenu(null), []);

  // ── 主文档的右键 ────────────────────────────────────────────
  useEffect(() => {
    const onContextMenu = (e: MouseEvent) => {
      // 捕获阶段就掐掉网页菜单：后面的组件处理器再 preventDefault 也没用，
      // 但原生菜单已经被浏览器收下了。
      e.preventDefault();
      // 点在菜单自己身上 → 不重开。
      if ((e.target as HTMLElement | null)?.closest?.(`.${CONTEXT_MENU_CLASS}`)) return;
      const ev = e;
      // 冒泡到 document 时才知道有没有组件自己接管（它们在目标阶段跑），
      // 所以推迟一个任务再决定开不开菜单。
      window.setTimeout(() => {
        if (isContextMenuHandled(ev)) return;
        setMenu(buildMenu(ev));
      }, 0);
    };
    document.addEventListener("contextmenu", onContextMenu, true);
    return () => document.removeEventListener("contextmenu", onContextMenu, true);
  }, []);

  // ── 工作台 iframe 的右键（跨源，只能靠 SDK 转发）────────────
  useEffect(() => {
    const onMessage = (e: MessageEvent) => {
      const d = e.data as
        | { source?: string; type?: string; x?: number; y?: number; text?: string; href?: string }
        | undefined;
      if (!d || d.source !== "hermix-workbench" || d.type !== "contextmenu") return;
      // iframe 坐标 → 视口坐标。
      const frame = Array.from(document.querySelectorAll("iframe")).find(
        (f) => f.contentWindow === e.source,
      );
      const rect = frame?.getBoundingClientRect();
      const x = (rect?.left ?? 0) + (d.x ?? 0);
      const y = (rect?.top ?? 0) + (d.y ?? 0);
      setMenu({
        x,
        y,
        items: buildItems({
          text: d.text ?? "",
          href: d.href,
          editable: false,
          image: null,
        }),
      });
    };
    window.addEventListener("message", onMessage);
    return () => window.removeEventListener("message", onMessage);
  }, []);

  if (!menu) return null;
  return <ContextMenu x={menu.x} y={menu.y} items={menu.items} onClose={close} />;
}

/** 按右键目标算出菜单项。 */
function buildMenu(e: MouseEvent): MenuState {
  const target = e.target instanceof HTMLElement ? e.target : null;
  const editable = editableOf(target);
  const image = target?.closest("img") as HTMLImageElement | null;
  const link = target?.closest("a[href]") as HTMLAnchorElement | null;
  const text = selectionText(editable);
  return {
    x: e.clientX,
    y: e.clientY,
    items: buildItems({
      text,
      href: link?.href,
      editable: editable !== null,
      image,
      editableEl: editable,
    }),
  };
}

function buildItems(opts: {
  text: string;
  href?: string;
  editable: boolean;
  image: HTMLImageElement | null;
  editableEl?: HTMLElement | null;
}): ContextMenuItem[] {
  const { text, href, editable, image, editableEl } = opts;
  const items: ContextMenuItem[] = [];
  const cap = captureSelection(editableEl ?? null);

  if (editable) {
    items.push({
      id: "cut",
      label: "剪切",
      icon: Scissors,
      disabled: !text,
      onSelect: () => {
        restoreSelection(cap);
        void writeClipboardText(text)
          .then(() => document.execCommand("delete"))
          .catch(() => toastError("剪切失败：无法写入剪贴板"));
      },
    });
    items.push({
      id: "copy",
      label: "复制",
      icon: Copy,
      disabled: !text,
      onSelect: () => {
        void writeClipboardText(text).catch(() => toastError("复制失败：无法写入剪贴板"));
      },
    });
    items.push({
      id: "paste",
      label: "粘贴",
      icon: ClipboardPaste,
      onSelect: () => {
        void navigator.clipboard
          .readText()
          .then((value) => {
            restoreSelection(cap);
            document.execCommand("insertText", false, value);
          })
          .catch(() => toastError("无法读取剪贴板，请用 Ctrl+V"));
      },
    });
    items.push({
      id: "select-all",
      label: "全选",
      icon: TextSelect,
      separator: true,
      onSelect: () => {
        restoreSelection(cap);
        document.execCommand("selectAll");
      },
    });
    return items;
  }

  if (text) {
    items.push({
      id: "copy",
      label: "复制",
      icon: Copy,
      onSelect: () => {
        void writeClipboardText(text).catch(() => toastError("复制失败：无法写入剪贴板"));
      },
    });
  }

  if (image) {
    items.push({
      id: "copy-image",
      label: "复制图片",
      icon: ImageIcon,
      onSelect: () => {
        void copyImageToClipboard(image).catch(() => toastError("复制图片失败"));
      },
    });
  }

  if (href) {
    items.push({
      id: "copy-link",
      label: "复制链接地址",
      icon: Link2,
      separator: items.length > 0,
      onSelect: () => {
        void writeClipboardText(href).catch(() => toastError("复制失败：无法写入剪贴板"));
      },
    });
    items.push({
      id: "open-link",
      label: "打开链接",
      icon: ExternalLink,
      onSelect: () => {
        window.open(href, "_blank", "noopener,noreferrer");
      },
    });
  }

  if (items.length === 0) {
    items.push({
      id: "copy",
      label: "复制",
      icon: Copy,
      disabled: true,
      onSelect: () => {},
    });
    items.push({
      id: "select-all",
      label: "全选",
      icon: TextSelect,
      onSelect: () => document.execCommand("selectAll"),
    });
  }

  return items;
}
