import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Search, Eye, Copy, FileText } from "lucide-react";

interface BrowserCommandResult {
  kind: string;
  ok: boolean;
  elapsedMs: number;
  result: unknown;
}

export function BrowserTab() {
  const [url, setUrl] = useState("");
  const [status, setStatus] = useState<"idle" | "loading" | "ready" | "error">("idle");
  const [pageTitle, setPageTitle] = useState<string | null>(null);
  const [history, setHistory] = useState<string[]>([]);
  const [screenshot, setScreenshot] = useState<string | null>(null);
  const [actionResult, setActionResult] = useState<string | null>(null);

  const handleNavigate = async () => {
    if (!url.trim()) return;
    setStatus("loading");
    setScreenshot(null);
    try {
      const resp = await invoke<BrowserCommandResult>("cua_browser_navigate", { url });
      if (resp.ok) {
        setPageTitle(url);
        setHistory((prev) => [url, ...prev].slice(0, 20));
        setStatus("ready");
      } else {
        setStatus("error");
      }
    } catch (e) {
      setStatus("error");
      setActionResult(`导航失败: ${e}`);
    }
  };

  const handleScreenshot = async () => {
    try {
      const resp = await invoke<BrowserCommandResult>("cua_browser_screenshot");
      if (resp.ok && resp.result && typeof resp.result === "object" && "png_base64" in (resp.result as Record<string, unknown>)) {
        const b64 = (resp.result as { png_base64: string }).png_base64;
        setScreenshot(`data:image/png;base64,${b64}`);
      } else {
        setActionResult("截图失败: 浏览器未运行");
      }
    } catch (e) {
      setActionResult(`截图失败: ${e}`);
    }
  };

  const handleEvaluate = async (expression: string, label: string) => {
    try {
      const resp = await invoke<BrowserCommandResult>("cua_browser_evaluate", { expression });
      if (resp.ok) {
        const text = typeof resp.result === "string" ? resp.result : JSON.stringify(resp.result, null, 2);
        setActionResult(`[${label}] ${text.slice(0, 500)}`);
      } else {
        setActionResult(`[${label}] 执行失败`);
      }
    } catch (e) {
      setActionResult(`[${label}] 错误: ${e}`);
    }
  };

  return (
    <div className="flex-1 flex flex-col overflow-hidden min-h-0">
      {/* URL Input */}
      <div className="px-3 py-3 border-b border-border-primary">
        <div className="flex items-center gap-2">
          <input
            value={url}
            onChange={(e) => setUrl(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleNavigate()}
            placeholder="输入网址..."
            className="flex-1 px-3 py-1.5 rounded-lg bg-bg-tertiary text-xs text-text-primary placeholder:text-text-faint outline-none focus:ring-1 focus:ring-accent/30"
          />
          <button
            onClick={handleNavigate}
            className="px-3 py-1.5 rounded-lg bg-accent text-white text-xs font-medium hover:opacity-90 transition-opacity"
          >
            前往
          </button>
        </div>
      </div>

      {/* Status Bar */}
      <div className="px-3 py-2 border-b border-border-primary flex items-center gap-2">
        <div className={`w-2 h-2 rounded-full ${
          status === "ready" ? "bg-green-400" :
          status === "loading" ? "bg-yellow-400 animate-pulse" :
          status === "error" ? "bg-red-400" : "bg-gray-400"
        }`} />
        <span className="text-[11px] text-text-muted">
          {status === "ready" ? pageTitle || "就绪" :
           status === "loading" ? "加载中..." :
           status === "error" ? "错误" : "未连接"}
        </span>
        {status === "ready" && (
          <button
            onClick={handleScreenshot}
            className="ml-auto px-2 py-0.5 rounded text-[10px] text-text-muted hover:bg-bg-hover transition-colors"
          >
            截图
          </button>
        )}
      </div>

      {/* Screenshot Preview */}
      {screenshot && (
        <div className="px-3 py-2 border-b border-border-primary">
          <img src={screenshot} alt="Screenshot" className="w-full rounded-lg border border-border-secondary" />
        </div>
      )}

      {/* Action Result */}
      {actionResult && (
        <div className="px-3 py-2 border-b border-border-primary">
          <pre className="text-[10.5px] text-text-muted whitespace-pre-wrap break-all max-h-32 overflow-y-auto">{actionResult}</pre>
          <button onClick={() => setActionResult(null)} className="text-[10px] text-accent hover:underline mt-1">清除</button>
        </div>
      )}

      {/* Quick Actions */}
      <div className="px-3 py-3 space-y-3">
        <div className="text-xs font-medium text-text-primary">快捷操作</div>
        <div className="grid grid-cols-2 gap-2">
          <QuickAction
            icon={<Search size={14} />}
            label="搜索页面"
            onClick={() => handleEvaluate("document.title", "页面标题")}
          />
          <QuickAction
            icon={<Eye size={14} />}
            label="元素检查"
            onClick={() => handleEvaluate("document.querySelector('h1')?.textContent || '(无 h1)'", "H1 检查")}
          />
          <QuickAction
            icon={<Copy size={14} />}
            label="复制内容"
            onClick={async () => {
              await handleEvaluate("document.body.innerText.slice(0, 5000)", "页面文本");
              if (actionResult) navigator.clipboard.writeText(actionResult);
            }}
          />
          <QuickAction
            icon={<FileText size={14} />}
            label="提取文本"
            onClick={() => handleEvaluate("document.body.innerText.slice(0, 2000)", "页面文本")}
          />
        </div>
      </div>

      {/* History */}
      {history.length > 0 && (
        <div className="px-3 py-3 border-t border-border-primary">
          <div className="text-xs font-medium text-text-primary mb-2">访问历史</div>
          <div className="space-y-1">
            {history.map((h, i) => (
              <button
                key={`hist-${h}-${i}`}
                onClick={() => setUrl(h)}
                className="block w-full text-left px-2 py-1 rounded text-[11px] text-text-muted hover:bg-bg-hover truncate transition-colors"
              >
                {h}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}

function QuickAction({
  icon,
  label,
  onClick,
}: {
  icon: React.ReactNode;
  label: string;
  onClick: () => void;
}) {
  return (
    <button
      onClick={onClick}
      className="flex items-center gap-2 px-3 py-2 rounded-lg bg-bg-tertiary hover:bg-bg-hover text-xs text-text-primary transition-colors"
    >
      {icon}
      {label}
    </button>
  );
}
