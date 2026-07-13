import { useState, useRef, useCallback } from "react";
import { Volume2, Square } from "lucide-react";
import { apiUrl } from "../api";

/**
 * L3: VoicePlayer — plays back TTS audio received from
 * `hermix-server /v1/voice/synthesize` (or, in the streaming case,
 * chunks pushed over WebSocket).
 *
 * The component is intentionally simple: a single HTML5 `<audio>`
 * element driven by a `src` URL. The front-end flow is:
 *
 * 1. Assistant replies with text.
 * 2. Front-end calls `POST /v1/voice/synthesize` with the reply text.
 * 3. Server returns `{ audio_base64, format, ... }`.
 * 4. We build a `data:audio/<format>;base64,...` URL and hand it to
 *    the `<audio>` element.
 *
 * For streaming TTS (sentence-by-sentence) we'd accumulate chunks
 * into a `MediaSource` buffer; that path is left for a follow-up
 * since the upstream `tts_streaming` pipeline already produces
 * `TextDelta → sentence` events, we just need a `PcmSink` wired into
 * the hermix-server handler. Out of scope for the first L3 pass.
 *
 * Design notes:
 * - "做好自己,不要去感知外部" — this component only consumes the
 *   existing hermix-server endpoints; no new external HTTP calls.
 * - Errors show a small tooltip so the user knows why TTS isn't
 *   working (e.g. API key not configured).
 */
interface VoicePlayerProps {
  /** The reply text to speak. Player is hidden if this is empty. */
  text: string;
  /** Optional voice id override (provider-specific). */
  voice?: string;
  /** Optional provider override; defaults to the server's choice. */
  provider?: string;
  /** Optional audio format; defaults to "mp3". */
  format?: string;
  /** Hide the player (e.g. user toggled voice off in settings). */
  hidden?: boolean;
}

export function VoicePlayer({
  text,
  voice,
  provider,
  format,
  hidden,
}: VoicePlayerProps) {
  const [playing, setPlaying] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const objectUrlRef = useRef<string | null>(null);

  const stop = useCallback(() => {
    audioRef.current?.pause();
    if (audioRef.current) audioRef.current.currentTime = 0;
    if (objectUrlRef.current) {
      URL.revokeObjectURL(objectUrlRef.current);
      objectUrlRef.current = null;
    }
    setPlaying(false);
  }, []);

  const play = useCallback(async () => {
    if (!text || loading) return;
    setLoading(true);
    setError(null);
    try {
      const res = await fetch(apiUrl("/v1/voice/synthesize"), {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text, voice, provider, format }),
      });
      if (!res.ok) {
        // TTS not configured or upstream provider failed.
        const errBody = await res.text().catch(() => "");
        let msg = `TTS 服务返回错误 (${res.status})`;
        if (errBody.includes("not set") || errBody.includes("not configured")) {
          msg = "TTS 未配置：请在设置中填入 OPENAI_API_KEY 或 ELEVENLABS_API_KEY";
        } else if (res.status >= 500) {
          msg = "TTS 服务暂时不可用";
        }
        setError(msg);
        setLoading(false);
        return;
      }
      const data = (await res.json()) as {
        audio_base64?: string;
        format?: string;
      };
      if (!data.audio_base64) {
        setError("TTS 返回空音频，请检查 API Key 配置");
        setLoading(false);
        return;
      }
      const mime = data.format || format || "mp3";
      const bytes = Uint8Array.from(atob(data.audio_base64), (c) =>
        c.charCodeAt(0),
      );
      const blob = new Blob([bytes], { type: `audio/${mime}` });
      if (objectUrlRef.current) URL.revokeObjectURL(objectUrlRef.current);
      const url = URL.createObjectURL(blob);
      objectUrlRef.current = url;
      const a = audioRef.current;
      if (a) {
        a.src = url;
        await a.play();
        setPlaying(true);
      }
    } catch {
      setError("无法连接 TTS 服务");
    } finally {
      setLoading(false);
    }
  }, [text, voice, provider, format, loading]);

  if (hidden || !text) return null;

  return (
    <div className="relative inline-flex">
      <button
        type="button"
        onClick={playing ? stop : play}
        title={error || (playing ? "停止播放" : "朗读回复")}
        className={`p-1 rounded transition-colors ${
          error
            ? "text-warning hover:text-warning"
            : playing
              ? "bg-accent text-white"
              : "text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
        }`}
      >
        {playing ? <Square size={12} /> : <Volume2 size={12} />}
        {loading && <span className="ml-1 text-[10px]">…</span>}
        <audio
          ref={audioRef}
          onEnded={() => {
            setPlaying(false);
            if (objectUrlRef.current) {
              URL.revokeObjectURL(objectUrlRef.current);
              objectUrlRef.current = null;
            }
          }}
          onError={() => setPlaying(false)}
        />
      </button>
      {error && (
        <span className="absolute bottom-full left-1/2 -translate-x-1/2 mb-1 whitespace-nowrap rounded bg-error px-2 py-0.5 text-[9px] text-white opacity-90 pointer-events-none z-10">
          {error}
        </span>
      )}
    </div>
  );
}
