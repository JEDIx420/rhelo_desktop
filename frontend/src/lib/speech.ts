type SpeechCommand = "speak_text" | "stop_speech";

interface SpeakArguments extends Record<string, unknown> {
  text: string;
  lang: string;
}

type SpeechArguments = SpeakArguments | Record<string, unknown>;

interface TauriWindow extends Window {
  __TAURI_INTERNALS__?: unknown;
}

const SUPPORTED_TTS_LANGUAGES = new Set(["en", "el", "he"]);
const normalizeLanguage = (language: string) =>
  language.toLowerCase().replace("_", "-").split("-")[0];

// WebKitGTK ships without the Web Speech API, so `window.speechSynthesis` is
// undefined on Linux. WKWebView on macOS provides it. Inside Tauri the native
// command path is used instead and never reaches these branches.
const getSpeechSynthesis = (): SpeechSynthesis | undefined =>
  typeof window !== "undefined" ? window.speechSynthesis : undefined;

export const ensureGreekVoice = (): boolean => {
  const synthesis = getSpeechSynthesis();
  if (!synthesis) return false;
  const voices = synthesis.getVoices();
  return voices.some((voice) => voice.lang.toLowerCase().includes("el"));
};

export async function invokeSpeech(command: SpeechCommand, args: SpeechArguments): Promise<void> {
  if ((window as TauriWindow).__TAURI_INTERNALS__ !== undefined) {
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke(command, args);
    return;
  }

  const synthesis = getSpeechSynthesis();

  if (command === "stop_speech") {
    synthesis?.cancel();
    return;
  }

  const { text, lang } = args as SpeakArguments;
  const normalizedLanguage = normalizeLanguage(lang);
  if (!SUPPORTED_TTS_LANGUAGES.has(normalizedLanguage)) {
    throw new Error("Rhelo TTS supports English, Hebrew, and Greek only.");
  }
  if (!synthesis) {
    throw new Error("Speech synthesis is not available in this browser.");
  }
  synthesis.cancel();
  const utterance = new SpeechSynthesisUtterance(text);
  const languages: Record<string, string> = {
    el: "el-GR", en: "en-US", he: "he-IL",
  };
  utterance.lang = languages[normalizedLanguage] || lang.replace("_", "-");
  synthesis.speak(utterance);
}
