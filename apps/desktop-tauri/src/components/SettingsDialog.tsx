import { Modal } from "./ui";
import { SettingsShell, resolvePage, type SettingsPage } from "./settings/SettingsShell";

/**
 * 设置入口：整幅浮层弹窗。
 *
 * 2026-10-19 用户拍板从整页改回弹窗：调完即关，不打断当前会话。外壳用
 * `ui/Modal`（焦点陷阱 / 点击外部关闭 / 关闭后还焦），内容仍由
 * `SettingsShell` 渲染（左导航 + 右内容，dialog 变体无"返回工作区"条）。
 *
 * `ConfigCategory` 仍从这里导出：`App` / `ChatView` / `Composer` / `Sidebar`
 * 的调用点只关心"落在哪一页"，不需要知道外壳形态。
 */
export type ConfigCategory = SettingsPage;

export interface SettingsDialogProps {
  open: boolean;
  onClose: () => void;
  /** 打开时落在哪一页；缺省由 `resolvePage` 决定（通用）。 */
  initialCategory?: ConfigCategory;
}

export function SettingsDialog({ open, onClose, initialCategory }: SettingsDialogProps) {
  return (
    <Modal
      open={open}
      onClose={onClose}
      /* 不画头部条：标题与关闭按钮并入面板自身布局（关闭钮浮在内容
         右上角），省掉 56px 空白，对齐参考产品的设置面板。 */
      ariaLabel="设置"
      width="panel"
      height="min(800px, calc(100vh - 3rem))"
      bodyClassName="min-h-0 flex-1 overflow-hidden p-0"
      /* Esc 交给 SettingsShell 做两级处理：先清空侧栏搜索，再关闭面板。 */
      closeOnEscape={false}
      /* 顶部 36px 条是拖动把手（SettingsShell 里的 data-modal-drag-handle）。 */
      draggable
    >
      <SettingsShell
        variant="dialog"
        onBack={onClose}
        initialPage={resolvePage(initialCategory)}
      />
    </Modal>
  );
}
