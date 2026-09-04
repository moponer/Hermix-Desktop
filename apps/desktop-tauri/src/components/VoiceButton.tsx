import { useState, useRef, useCallback } from "react";
import { Mic } from "lucide-react";
import * as api from "../api";

interface VoiceButtonProps {
  onTranscribe: (text: string) => void;
  disabled?: boolean;
}

/**
 * Convert a webm audio Blob to 16kHz mono s16 PCM base64.
 *
 * Uses the Web Audio API to decode the browser's native recording
 * format (audio/webm; Opus codec) into raw PCM samples, then
 * resamples to 16 kHz mono and encodes as signed 16-bit little-endian.
 *
 * This is necessary because the backend's `transcribe_pcm` endpoint
 * expects raw s16 PCM — not a webm container.
 */
async function webmBlobToPcmBase64(blob: Blob): Promise<string> {
  const arrayBuffer = await blob.arrayBuffer();
  const audioContext = new AudioContext({ sampleRate: 16000 });
  try {
    const audioBuffer = await audioContext.decodeAudioData(arrayBuffer);
    // Mix down to mono: average all channels.
    const numChannels = audioBuffer.numberOfChannels;
    const length = audioBuffer.length;
    const mono = new Float32Array(length);
    for (let ch = 0; ch < numChannels; ch++) {
      const channelData = audioBuffer.getChannelData(ch);
      for (let i = 0; i < length; i++) {
        mono[i] += channelData[i] / numChannels;
      }
    }
    // If the AudioContext didn't already resample to 16kHz (it should
    // because we set sampleRate: 16000), do a simple linear resample.
    const targetRate = 16000;
    const sourceRate = audioBuffer.sampleRate;
    let pcm32: Float32Array;
    if (sourceRate !== targetRate) {
      const ratio = sourceRate / targetRate;
      const newLength = Math.round(length / ratio);
      pcm32 = new Float32Array(newLength);
      for (let i = 0; i < newLength; i++) {
        const srcIdx = i * ratio;
        const idx0 = Math.floor(srcIdx);
        const idx1 = Math.min(idx0 + 1, length - 1);
        const frac = srcIdx - idx0;
        pcm32[i] = mono[idx0] * (1 - frac) + mono[idx1] * frac;
      }
    } else {
      pcm32 = mono;
    }
    // Convert float32 [-1, 1] to int16 [-32768, 32767]
    const pcm16 = new Int16Array(pcm32.length);
    for (let i = 0; i < pcm32.length; i++) {
      const s = Math.max(-1, Math.min(1, pcm32[i]));
      pcm16[i] = s < 0 ? s * 0x8000 : s * 0x7fff;
    }
    // Encode as base64
    const bytes = new Uint8Array(pcm16.buffer);
    let binary = "";
    const chunkSize = 0x8000;
    for (let i = 0; i < bytes.length; i += chunkSize) {
      binary += String.fromCharCode.apply(null, Array.from(bytes.subarray(i, i + chunkSize)) as unknown as number[]);
    }
    return btoa(binary);
  } finally {
    audioContext.close();
  }
}

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
        try {
          const pcmBase64 = await webmBlobToPcmBase64(blob);
          // stdio bridge: same /v1/voice/transcribe endpoint, just
          // routed through `invoke("backend_http_request")` instead
          // of `fetch('http://127.0.0.1:3000/...')`. Body now goes
          // through the bridge's JSON envelope so it has to be a real
          // JSON value, not a string — the old `headers` form is
          // unnecessary (fetchJSON sets content-type when body is set).
          const { text } = await api.fetchJSON<{ text?: string }>(
            "/v1/voice/transcribe",
            {
              method: "POST",
              body: JSON.stringify({
                pcm_base64: pcmBase64,
                sample_rate: 16000,
                language: "zh",
              }),
            },
          );
          if (text) onTranscribe(text);
        } catch {
          // bridge round-trip failed / mic unsupported / permission denied
        }
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
      <Mic className="icon-sm" />
      <span>{active ? "停止" : "语音"}</span>
    </button>
  );
}
