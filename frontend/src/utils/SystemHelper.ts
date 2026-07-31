import { open } from "@tauri-apps/plugin-shell";

// Linux has no single speech settings panel to open — voices come from whatever
// synthesiser speech-dispatcher is configured to use — so it gets instructions
// instead of a URI.
const getSettingsUri = () => {
  const platform = navigator.userAgent.toLowerCase();
  if (platform.includes("win")) {
    return "ms-settings:speech";
  }
  if (platform.includes("linux")) {
    return null;
  }
  return "x-apple.systempreferences:com.apple.preference.universalaccess?Speech";
};

const LINUX_INSTRUCTIONS =
  "Rhelo speaks through speech-dispatcher on Linux. Install a synthesiser with " +
  "the voices you need, for example:\n\n    sudo apt install espeak-ng\n\n" +
  "then restart Rhelo.";

export const openSpeechSettings = async () => {
  const uri = getSettingsUri();
  if (!uri) {
    alert(LINUX_INSTRUCTIONS);
    return;
  }
  try {
    await open(uri);
  } catch (err) {
    console.error("Failed to open speech settings:", err);
    alert("Please open your system speech settings and install a Greek voice pack.");
  }
};
