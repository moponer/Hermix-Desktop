import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type Step = "welcome" | "license" | "dir" | "installing" | "done";

interface InstallProgress {
  phase: "extract" | "shortcut" | "register" | "done";
  received: number;
  total: number;
}

interface InstallerMeta {
  version: string;
  default_dir: string;
}

const LICENSE_FALLBACK = "Hermix Desktop 许可协议\n\n完整许可文本见安装目录内 license.rtf。";

export function App() {
  const [step, setStep] = useState<Step>("welcome");
  const [meta, setMeta] = useState<InstallerMeta>({ version: "", default_dir: "" });
  const [licenseText, setLicenseText] = useState("正在加载许可协议…");
  const [accepted, setAccepted] = useState(false);
  const [dir, setDir] = useState("");
  const [shortcut, setShortcut] = useState(true);
  const [runAfter, setRunAfter] = useState(true);
  const [progress, setProgress] = useState<InstallProgress | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<InstallerMeta>("installer_meta")
      .then((m) => {
        setMeta(m);
        setDir((d) => d || m.default_dir);
      })
      .catch((e) => setError(String(e)));
    invoke<string>("installer_license").then(setLicenseText).catch(() => setLicenseText(LICENSE_FALLBACK));
    const un = listen<InstallProgress>("installer://progress", (e) => setProgress(e.payload));
    return () => {
      un.then((f) => f()).catch(() => {});
    };
  }, []);

  const closeWindow = useCallback(() => {
    void getCurrentWindow().close();
  }, []);

  const browse = useCallback(async () => {
    try {
      const picked = await invoke<string | null>("installer_pick_dir");
      if (picked) setDir(picked);
    } catch (e) {
      setError(String(e));
    }
  }, []);

  const startInstall = useCallback(async () => {
    setError(null);
    setStep("installing");
    try {
      await invoke("installer_install", { installDir: dir, desktopShortcut: shortcut });
      setStep("done");
    } catch (e) {
      setError(String(e));
      setStep("dir"); // 回到目录页让用户重试；错误横幅就地显示
    }
  }, [dir, shortcut]);

  const finish = useCallback(async () => {
    try {
      await invoke("installer_finish", { runNow: runAfter });
    } catch {
      closeWindow();
    }
  }, [runAfter, closeWindow]);

  const underProgramFiles = /^[a-z]:\\program files/i.test(dir);
  const pct =
    progress && progress.total > 0 ? Math.min(100, Math.round((progress.received / progress.total) * 100)) : null;

  const progressLabel =
    progress?.phase === "extract"
      ? pct !== null
        ? `正在解压文件… ${pct}%`
        : "正在解压文件…"
      : progress?.phase === "shortcut"
        ? "正在创建快捷方式…"
        : progress?.phase === "register"
          ? "正在登记卸载信息…"
          : "正在准备…";

  return (
    <div className="installer-root">
      {/* 自绘标题栏（decorations:false）；拖拽区 + 关闭钮 */}
      <div className="installer-titlebar" data-tauri-drag-region>
        <span className="title" data-tauri-drag-region>Hermix 安装程序</span>
        <span className="spacer" />
        <button onClick={closeWindow} aria-label="关闭">✕</button>
      </div>

      <div className="installer-body">
        {step === "welcome" && (
          <>
            <div className="logo-row">
              <div className="logo-fallback">H</div>
              <h1>欢迎安装 Hermix{meta.version ? ` v${meta.version}` : ""}</h1>
            </div>
            <p className="sub">
              全新的便携 AI 助手。向导将引导你完成安装——程序与数据都在你选择的目录里，随时整目录删除即可卸载。
            </p>
          </>
        )}

        {step === "license" && (
          <>
            <h1>许可协议</h1>
            <p className="sub">安装前请阅读并接受以下协议。</p>
            <div className="license-box">{licenseText}</div>
            <label className="check-row">
              <input type="checkbox" checked={accepted} onChange={(e) => setAccepted(e.target.checked)} />
              我已阅读并接受本协议
            </label>
          </>
        )}

        {step === "dir" && (
          <>
            <h1>安装位置</h1>
            <p className="sub">选择 Hermix 的安装目录。程序、sidecar 与更新缓存都放在这里。</p>
            <div className="dir-row">
              <input value={dir} onChange={(e) => setDir(e.target.value)} spellCheck={false} />
              <button onClick={() => void browse()}>浏览…</button>
            </div>
            <p className="dir-hint">
              默认安装到用户目录（%LOCALAPPDATA%\Programs\Hermix），自动更新无需管理员权限。
            </p>
            {underProgramFiles && (
              <div className="dir-warn">
                你选择了 Program Files 下的目录：后续自动更新将需要管理员权限。个人使用建议保持默认目录。
              </div>
            )}
            <label className="check-row" style={{ marginTop: 12 }}>
              <input type="checkbox" checked={shortcut} onChange={(e) => setShortcut(e.target.checked)} />
              创建桌面快捷方式
            </label>
            {error && <div className="error-banner">{error}</div>}
          </>
        )}

        {step === "installing" && (
          <>
            <h1>正在安装</h1>
            <div className="progress-track">
              <div className="progress-fill" style={{ width: pct !== null ? `${pct}%` : "40%" }} />
            </div>
            <p className="progress-label">{progressLabel}</p>
          </>
        )}

        {step === "done" && (
          <>
            <div className="done-icon">✅</div>
            <h1>安装完成</h1>
            <p className="sub">Hermix 已安装到 {dir}</p>
            <div style={{ marginTop: 16 }}>
              <label className="check-row">
                <input type="checkbox" checked={runAfter} onChange={(e) => setRunAfter(e.target.checked)} />
                立即运行 Hermix
              </label>
            </div>
          </>
        )}
      </div>

      <div className="installer-footer">
        {step === "license" && (
          <button className="btn btn-ghost" onClick={() => setStep("welcome")}>上一步</button>
        )}
        {step === "dir" && (
          <button className="btn btn-ghost" onClick={() => setStep("license")}>上一步</button>
        )}
        <span className="spacer" />
        {step === "welcome" && (
          <button className="btn btn-primary" onClick={() => setStep("license")}>下一步</button>
        )}
        {step === "license" && (
          <button className="btn btn-primary" disabled={!accepted} onClick={() => setStep("dir")}>下一步</button>
        )}
        {step === "dir" && (
          <button className="btn btn-primary" disabled={!dir.trim()} onClick={() => void startInstall()}>
            安装
          </button>
        )}
        {step === "done" && (
          <button className="btn btn-primary" onClick={() => void finish()}>完成</button>
        )}
      </div>
    </div>
  );
}
