/**
 * 手机端语音识别配置（设置 → 管理 → 手机语音识别）。
 *
 * 配置存在后端 `$HERMIX_HOME/voice_stt.json`（单一真相源，见
 * `hermix-server/src/dashboard/voice_stt.rs`）；每条 WS 会话建立时经
 * 密封帧 `voice.stt_config` 下发给手机。手机把录音直接发给 provider、
 * 识别出的文字走既有加密通道——音频与文字都不经 relay 明文。
 *
 * provider 无关：endpoint 只要收 POST 音频、返回 `{text}` 即可
 * （MiniMax / OpenAI 兼容 / SiliconFlow…）。关闭开关 = 手机端长按说话
 * 提示「桌面端未配置语音识别，仅支持文字发送」。
 */
import { useCallback, useEffect, useState } from "react";
import { Mic } from "lucide-react";
import { fetchJSON } from "../../api/rest";
import { Button, IconTile, Input, Switch } from "../ui";
import { SAVE, SAVED, SAVING } from "../strings";

interface SttConfig {
  enabled: boolean;
  endpoint: string;
  api_key: string;
  model: string | null;
  language: string | null;
  audio_mime: string;
  extra_headers: Record<string, string> | null;
}

const EMPTY: SttConfig = {
  enabled: false,
  endpoint: "",
  api_key: "",
  model: null,
  language: "zh",
  audio_mime: "audio/mpeg",
  extra_headers: null,
};

export function VoiceTab() {
  const [cfg, setCfg] = useState<SttConfig>(EMPTY);
  const [loaded, setLoaded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    const ac = new AbortController();
    fetchJSON<SttConfig>("/api/voice/stt", { signal: ac.signal })
      .then((c) => setCfg({ ...EMPTY, ...c }))
      .catch((e) =>
        setMsg({
          ok: false,
          text: `读取语音识别配置失败：${e instanceof Error ? e.message : String(e)}。请确认后端服务已启动后重试。`,
        }),
      )
      .finally(() => setLoaded(true));
    return () => ac.abort();
  }, []);

  const save = useCallback(async () => {
    setBusy(true);
    setMsg(null);
    try {
      await fetchJSON("/api/voice/stt", {
        method: "PUT",
        headers: { "content-type": "application/json" },
        body: JSON.stringify(cfg),
      });
      setMsg({ ok: true, text: `${SAVED}。手机端下次连接会话时生效。` });
    } catch (e) {
      /* 真实原因不能只写「保存失败」：用户要知道是端点不合法还是网络不通。 */
      setMsg({
        ok: false,
        text: `保存失败：${e instanceof Error ? e.message : String(e)}。请检查端点 URL 与 API Key 后重试。`,
      });
    } finally {
      setBusy(false);
    }
  }, [cfg]);

  if (!loaded)
    return <div className="p-4 text-sm opacity-60">读取中…</div>;

  return (
    <div className="space-y-4">
      <div className="space-y-4">
        <div className="flex items-center gap-3">
          <IconTile icon={<Mic className="icon-md" />} tone="orange" />
          <Switch
            checked={cfg.enabled}
            onChange={(v) => setCfg({ ...cfg, enabled: v })}
            label="开启手机端语音识别"
          />
          <span className="text-sm font-medium">
            开启手机端语音识别（识别在云 provider 上，音频不经 relay）
          </span>
        </div>

        <Input
          size="lg"
          label="识别端点 URL（必须 https）"
          hint="指向云 provider 的转写接口，例如 https://api.minimax.chat/v1/audio/transcriptions"
          placeholder="https://api.minimax.chat/v1/audio/transcriptions"
          value={cfg.endpoint}
          onChange={(e) => setCfg({ ...cfg, endpoint: e.target.value })}
        />

        <Input
          size="lg"
          type="password"
          label="API Key（仅存本机与手机端，经端到端加密下发）"
          value={cfg.api_key}
          onChange={(e) => setCfg({ ...cfg, api_key: e.target.value })}
        />

        <div className="grid grid-cols-2 gap-3">
          <Input
            size="lg"
            label="模型（可选）"
            hint="留空 = 用 provider 默认模型"
            value={cfg.model ?? ""}
            onChange={(e) => setCfg({ ...cfg, model: e.target.value || null })}
          />
          <Input
            size="lg"
            label="语言（如 zh）"
            hint="留空 = 由 provider 自动识别"
            value={cfg.language ?? ""}
            onChange={(e) => setCfg({ ...cfg, language: e.target.value || null })}
          />
        </div>

        <Input
          size="lg"
          label="音频 MIME（与手机端录音格式配套，默认 audio/mpeg）"
          value={cfg.audio_mime}
          onChange={(e) => setCfg({ ...cfg, audio_mime: e.target.value || "audio/mpeg" })}
        />

        <Input
          size="lg"
          label="附加请求头（JSON，可选；如 provider 要求特殊鉴权头）"
          hint={'形如 {"x-custom":"value"}；JSON 不合法时保存会被拒绝'}
          placeholder='{"x-custom":"value"}'
          value={cfg.extra_headers ? JSON.stringify(cfg.extra_headers) : ""}
          onChange={(e) => {
            try {
              setCfg({ ...cfg, extra_headers: e.target.value ? JSON.parse(e.target.value) : null });
            } catch {
              // 输入中的半截 JSON 不报错，保存时才校验
              setCfg({ ...cfg, extra_headers: { "x-invalid-json": e.target.value } });
            }
          }}
        />

        <div className="flex items-center gap-3">
          <Button variant="accent" loading={busy} onClick={() => void save()}>
            {busy ? SAVING : SAVE}
          </Button>
          {msg && (
            <span
              /* 文字用语义四色的**文字安全档**（--color-text-*），不是原始
                 语义色 —— 原始档是给图标和填充用的，作小字会掉到 4.5:1 以下。 */
              className={`text-sm ${msg.ok ? "text-text-success" : "text-text-error"}`}
            >
              {msg.text}
            </span>
          )}
        </div>

        <p className="text-xs opacity-50">
          手机端在聊天页长按说话即可录音发送；未开启时手机端会显示「桌面端未配置语音识别，仅支持文字发送」。
        </p>
      </div>
    </div>
  );
}
