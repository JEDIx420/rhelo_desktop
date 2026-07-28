type SessionSaveFlusher = () => Promise<void>;

let activeFlusher: SessionSaveFlusher | null = null;

export function registerSessionSaveFlusher(flusher: SessionSaveFlusher): () => void {
  activeFlusher = flusher;
  return () => {
    if (activeFlusher === flusher) activeFlusher = null;
  };
}

export async function flushActiveSessionEdits(): Promise<void> {
  await activeFlusher?.();
}
