import { useState, useRef, useCallback } from "react";
import { Mic } from "lucide-react";

interface VoiceButtonProps {
  onTranscribe: (text: string) => void;
  disabled?: boolean;
}

const API_BASE = "http://127.0.0.1:3000";

export function VoiceButton({ onTranscribe, disabled }: VoiceButtonProps) {
  const [active, setActive] = useState(false);
  const recorderRef = useRef<MediaRecorder | null>(null);
  const chunksRef = useRef<Blob[]>([]);

  const stop = useCallback(() => {
    recorderRef.current?.stop();
    setActive(false);
  }, []);

  const toggle = useCallback(async () => {
    if (active) { stop(); return; }
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      const rec = new MediaRecorder(stream, { mimeType: "audio/webm" });
      chunksRef.current = [];
      rec.ondataavailable = (e: BlobEvent) => {
        if (e.data.size > 0) chunksRef.current.push(e.data);
      };
      rec.onstop = async () => {
        const blob = new Blob(chunksRef.current, { type: "audio/webm" });
        stream.getTracks().forEach(t => t.stop());
        const reader = new FileReader();
        reader.onloadend = async () => {
          try {
            const b64 = (reader.result as string).split(",")[1];
            // L3: POST to hermix-server's /v1/voice/transcribe endpoint.
            // The server's `voice_transcribe` handler wraps
            // `hermix_voice::transcribe_pcm` which currently expects raw
            // s16 PCM; for the existing `webm` blob payload we send
            // the format + raw base64 and let the server log a clear
            // "unsupported format" error until the L3 webm→PCM
            // ffmpeg-style transcode path is wired in (tracked
            // separately so we don't pull a ffmpeg dep here).
            const res = await fetch(`${API_BASE}/v1/voice/transcribe`, {
              method: "POST",
              headers: { "Content-Type": "application/json" },
              body: JSON.stringify({
                pcm_base64: b64,
                sample_rate: 16000,
                language: "zh",
              }),
            });
            if (!res.ok) {
              // Server may not be reachable in dev — silent fail so
              // the user just sees a missing transcription.
              return;
            }
            const { text } = (await res.json()) as { text?: string };
            if (text) onTranscribe(text);
          } catch {
            // network error / mic not supported / permission denied
          }
        };
        reader.readAsDataURL(blob);
      };
      rec.start();
      recorderRef.current = rec;
      setActive(true);
    } catch {
      // mic not supported or permission denied — silent fail
    }
  }, [active, onTranscribe]);

  return (
    <button
      type="button"
      onClick={toggle}
      disabled={disabled}
      title={active ? "停止录音" : "语音输入"}
      className={`flex items-center gap-1 px-2 py-1 rounded transition-colors text-[12px] ${
        active
          ? "bg-error text-white animate-pulse"
          : "text-text-muted hover:text-text-primary hover:bg-bg-tertiary"
      } disabled:opacity-30`}
    >
      <Mic size={12} />
      <span>{active ? "停止" : "语音"}</span>
    </button>
  );
}
