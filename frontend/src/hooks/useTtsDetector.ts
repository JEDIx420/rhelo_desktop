import { useEffect, useState } from "react";

type TtsStatus = "checking" | "ready" | "missing";

const normalizeLanguage = (language: string) =>
  language.toLowerCase().replace("_", "-");

// WebKitGTK ships without the Web Speech API, so `window.speechSynthesis` is
// undefined on Linux. WKWebView on macOS provides it.
const getSpeechSynthesis = (): SpeechSynthesis | undefined =>
  typeof window !== "undefined" ? window.speechSynthesis : undefined;

const isTauri = () =>
  typeof window !== "undefined" &&
  (window as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__ !== undefined;

export const useTtsDetector = (lang: string = "el-GR") => {
  const [status, setStatus] = useState<TtsStatus>("checking");

  useEffect(() => {
    let cancelled = false;
    let retries = 0;
    const maxRetries = 10;

    // Inside the desktop app, speech runs through the native engine rather than
    // the browser, so the browser voice list is the wrong thing to inspect.
    if (isTauri()) {
      (async () => {
        try {
          const { invoke } = await import("@tauri-apps/api/core");
          const available = await invoke<boolean>("has_tts_voice", { lang });
          if (!cancelled) setStatus(available ? "ready" : "missing");
        } catch (error) {
          console.error("[Rhelo] Native TTS voice check failed", error);
          if (!cancelled) setStatus("missing");
        }
      })();
      return () => {
        cancelled = true;
      };
    }

    const synthesis = getSpeechSynthesis();
    if (!synthesis) {
      setStatus("missing");
      return;
    }

    const matchesLanguage = (voiceLang: string) => {
      const normalizedVoice = normalizeLanguage(voiceLang);
      const normalizedTarget = normalizeLanguage(lang);
      const targetBase = normalizedTarget.split("-")[0];
      const voiceBase = normalizedVoice.split("-")[0];
      return (
        normalizedVoice === normalizedTarget ||
        voiceBase === targetBase ||
        normalizedVoice.startsWith(`${targetBase}-`)
      );
    };

    const checkVoices = () => {
      if (cancelled) return false;
      const voices = synthesis.getVoices();
      if (voices.some((voice) => matchesLanguage(voice.lang) || /greek|stefanos/i.test(voice.name))) {
        setStatus("ready");
        return true;
      }
      return false;
    };

    const settleMissing = () => {
      if (!cancelled) setStatus((current) => (current === "checking" ? "missing" : current));
    };

    if (checkVoices()) {
      return () => {
        cancelled = true;
      };
    }

    const interval = window.setInterval(() => {
      if (checkVoices()) {
        window.clearInterval(interval);
        return;
      }

      retries += 1;
      if (retries >= maxRetries) {
        window.clearInterval(interval);
        settleMissing();
      }
    }, 500);

    const handleVoicesChanged = () => {
      if (checkVoices()) {
        window.clearInterval(interval);
      }
    };

    synthesis.addEventListener("voiceschanged", handleVoicesChanged);

    return () => {
      cancelled = true;
      window.clearInterval(interval);
      synthesis.removeEventListener("voiceschanged", handleVoicesChanged);
    };
  }, [lang]);

  return status;
};
