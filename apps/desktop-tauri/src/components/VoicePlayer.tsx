import { useState, useRef, useCallback } from "react";
import { Volume2, Square } from "lucide-react";

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
 * - Errors are silent (button just doesn't react) so a misconfigured
 *   TTS provider doesn't break the chat UI.
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

const API_BASE = "http://127.0.0.1:3000";

export function VoicePlayer({
  text,
  voice,
  provider,
  format,
  hidden,
}: VoicePlayerProps) {
  const [playing, setPlaying] = useState(false);
  const [loading, setLoading] = useState(false);
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
    try {
      const res = await fetch(`${API_BASE}/v1/voice/synthesize`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text, voice, provider, format }),
      });
      if (!res.ok) {
        // TTS not configured or upstream provider failed — silent.
        setLoading(false);
        return;
      }
      const data = (await res.json()) as {
        audio_base64?: string;
        format?: string;
      };
      if (!data.audio_base64) {
        // No audio bytes (server returns empty payload when no
        // provider is configured). Silent — UI just doesn't react.
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
      // network error — silent
    } finally {
      setLoading(false);
    }
  }, [text, voice, provider, format, loading]);

  if (hidden || !text) return null;

  return (
    <button
      type="button"
      onClick={playing ? stop : play}
      title={playing ? "停止播放" : "朗读回复"}
      className={`p-1 rounded transition-colors ${
        playing
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
  );
}
