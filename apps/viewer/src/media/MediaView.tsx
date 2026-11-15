// 媒体预览（音视频）。
//
// **为什么不走字节通道**：媒体文件动辄几百 MB，走「读字节 → base64 →
// IPC → 前端再解回字节」会把整份文件搬两遍，内存和时间都不可接受。
// Tauri 的 asset 协议直接支持 Range（206 + Accept-Ranges），媒体元素
// 可以边下边播、还能拖进度条——这正是它的原生场景。
//
// 代价与边界：
//   - 需要在 tauri.conf.json 里开 assetProtocol（scope 放开：viewer 本来
//     就是文件浏览器，能读任意路径，asset 协议没有引入新权限）。
//   - 浏览器解不了的容器（mkv / avi / wmv / flv / mpg）会触发 error 事件，
//     此时落回「用系统程序打开」，而不是留一个黑框。

import { useMemo, useState } from "react";
import { convertFileSrc } from "@tauri-apps/api/core";
import { ExternalLink } from "lucide-react";

/** 视频容器扩展名。 */
const VIDEO_EXTS = new Set([
  "mp4", "m4v", "mov", "webm", "ogv", "mkv", "avi", "wmv", "flv", "mpg", "mpeg", "3gp",
]);

/** 音频容器扩展名。 */
const AUDIO_EXTS = new Set([
  "mp3", "wav", "m4a", "aac", "flac", "ogg", "oga", "opus", "wma", "aiff", "mid",
]);

/** 这个文件是不是媒体？ */
export function isMediaFile(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return VIDEO_EXTS.has(ext) || AUDIO_EXTS.has(ext);
}

function isAudio(name: string): boolean {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return AUDIO_EXTS.has(ext);
}

/**
 * 媒体打不开有两种根因，不能混成一句话：
 * - `unsupported`：文件本身解不了（容器/编码 WebView 不支持）→ 换系统播放器有意义
 * - `load`：请求没到位（协议被拦、文件被占、权限不足）→ 说成「解不了」是误导
 */
type MediaFailure = "unsupported" | "load";

function classify(err: MediaError | null): MediaFailure {
  const code = err?.code;
  return code === MediaError.MEDIA_ERR_DECODE || code === MediaError.MEDIA_ERR_SRC_NOT_SUPPORTED
    ? "unsupported"
    : "load";
}

export function MediaView({
  path,
  name,
  onFallback,
}: {
  path: string;
  name: string;
  onFallback: (reason: string) => void;
}) {
  const [failure, setFailure] = useState<MediaFailure | null>(null);
  const src = useMemo(() => convertFileSrc(path), [path]);
  const audio = isAudio(name);
  const ext = name.split(".").pop()?.toUpperCase() ?? "";

  if (failure) {
    return (
      <div className="preview-state-center">
        <div className="meta-card">
          <p className="meta-card-msg">
            {failure === "unsupported"
              ? `这个浏览器解不了该媒体格式（${ext}）。`
              : `媒体加载失败（${ext}），文件可能正被其他程序占用，或读取协议被拦。`}
          </p>
          <div className="meta-card-actions">
            <button
              type="button"
              className="tb-btn"
              onClick={() => onFallback("用系统默认播放器打开")}
            >
              <ExternalLink className="icon-sm" /> 用系统程序打开
            </button>
          </div>
        </div>
      </div>
    );
  }

  if (audio) {
    return (
      <div className="media-wrap is-audio">
        <audio
          src={src}
          controls
          onError={(e) => setFailure(classify(e.currentTarget.error))}
          aria-label={name}
        />
      </div>
    );
  }

  return (
    <div className="media-wrap">
      <video
        src={src}
        controls
        onError={(e) => setFailure(classify(e.currentTarget.error))}
        aria-label={name}
      />
    </div>
  );
}
