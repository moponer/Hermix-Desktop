import { useRef, useCallback } from "react";
import { FileText } from "lucide-react";

interface VisionButtonProps {
  /**
   * 直接传 File 数组，零拷贝。
   * 父组件负责压缩 + blob URL。
   */
  onFilesAttach: (files: File[]) => void;
  disabled?: boolean;
}

// 删 FileReader.readAsDataURL — 同步阻塞主线程 1.5s/8MB。
// 直接传 File 引用，base64 转换/压缩/预览全部后置。
const ACCEPT_LIST = "*/*";

export function VisionButton({ onFilesAttach, disabled }: VisionButtonProps) {
  const inputRef = useRef<HTMLInputElement>(null);

  const handleChange = useCallback(
    (e: React.ChangeEvent<HTMLInputElement>) => {
      const files = Array.from(e.target.files ?? []);
      if (files.length === 0) return;
      onFilesAttach(files);
      e.target.value = ""; // 重置以便重复选同一文件
    },
    [onFilesAttach],
  );

  return (
    <>
      <input
        ref={inputRef}
        type="file"
        accept={ACCEPT_LIST}
        multiple
        className="hidden"
        onChange={handleChange}
      />
      <button
        type="button"
        onClick={() => inputRef.current?.click()}
        disabled={disabled}
        title="上传文档或图片（视觉理解）"
        className="flex items-center gap-1 px-2 py-1 rounded text-text-muted hover:text-text-primary hover:bg-bg-tertiary disabled:opacity-30 transition-colors text-[12px]"
      >
        <FileText size={12} />
        <span>文档图片</span>
      </button>
    </>
  );
}