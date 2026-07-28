export interface SessionSaveSnapshot {
  sessionId: string;
  title: string;
  content: string;
  generation: number;
}

interface PendingSessionSave {
  snapshot: SessionSaveSnapshot;
  timer: ReturnType<typeof setTimeout> | null;
}

interface SessionSaveCoordinatorOptions {
  delayMs?: number;
  persist: (snapshot: SessionSaveSnapshot) => Promise<void>;
}

export type SessionSaveEvent =
  | { type: "persisted"; snapshot: SessionSaveSnapshot }
  | { type: "error"; snapshot: SessionSaveSnapshot; error: Error };

export class SessionSaveCoordinator {
  private readonly delayMs: number;
  private readonly persist: SessionSaveCoordinatorOptions["persist"];
  private readonly listeners = new Set<(event: SessionSaveEvent) => void>();
  private readonly generations = new Map<string, number>();
  private readonly pending = new Map<string, PendingSessionSave>();
  private readonly queues = new Map<string, Promise<void>>();
  private readonly deletedSessionIds = new Set<string>();
  private disposed = false;

  constructor(options: SessionSaveCoordinatorOptions) {
    this.delayMs = options.delayMs ?? 1500;
    this.persist = options.persist;
  }

  subscribe(listener: (event: SessionSaveEvent) => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  schedule(input: Omit<SessionSaveSnapshot, "generation">): SessionSaveSnapshot {
    if (this.disposed || this.deletedSessionIds.has(input.sessionId)) {
      throw new Error("Cannot schedule a save for an inactive study session.");
    }

    const generation = (this.generations.get(input.sessionId) ?? 0) + 1;
    this.generations.set(input.sessionId, generation);
    const snapshot = Object.freeze({ ...input, generation });
    const previous = this.pending.get(input.sessionId);
    if (previous?.timer) clearTimeout(previous.timer);

    const timer = setTimeout(() => {
      void this.enqueuePending(input.sessionId).catch(() => undefined);
    }, this.delayMs);
    this.pending.set(input.sessionId, { snapshot, timer });
    return snapshot;
  }

  async flush(sessionId: string): Promise<void> {
    await this.enqueuePending(sessionId);
    await this.queues.get(sessionId);
  }

  async flushAll(): Promise<void> {
    const sessionIds = new Set([...this.pending.keys(), ...this.queues.keys()]);
    await Promise.all([...sessionIds].map((sessionId) => this.flush(sessionId)));
  }

  discardSession(sessionId: string): void {
    this.deletedSessionIds.add(sessionId);
    const pending = this.pending.get(sessionId);
    if (pending?.timer) clearTimeout(pending.timer);
    this.pending.delete(sessionId);
  }

  restoreSession(sessionId: string): void {
    this.deletedSessionIds.delete(sessionId);
  }

  hasPending(sessionId?: string): boolean {
    if (sessionId) return this.pending.has(sessionId);
    return this.pending.size > 0;
  }

  dispose(): void {
    this.disposed = true;
    for (const pending of this.pending.values()) {
      if (pending.timer) clearTimeout(pending.timer);
    }
    this.pending.clear();
  }

  private async enqueuePending(sessionId: string): Promise<void> {
    const pending = this.pending.get(sessionId);
    if (!pending) return;
    if (pending.timer) clearTimeout(pending.timer);
    this.pending.delete(sessionId);

    const previous = this.queues.get(sessionId) ?? Promise.resolve();
    const operation = previous
      .catch(() => undefined)
      .then(async () => {
        const { snapshot } = pending;
        if (this.deletedSessionIds.has(sessionId)) return;
        if (snapshot.generation < (this.generations.get(sessionId) ?? 0)) return;

        try {
          await this.persist(snapshot);
          this.emit({ type: "persisted", snapshot });
        } catch (cause) {
          const error = cause instanceof Error ? cause : new Error(String(cause));
          if (
            !this.deletedSessionIds.has(sessionId)
            && snapshot.generation === this.generations.get(sessionId)
          ) {
            this.pending.set(sessionId, { snapshot, timer: null });
          }
          this.emit({ type: "error", error, snapshot });
          throw error;
        }
      });

    this.queues.set(sessionId, operation);
    try {
      await operation;
    } finally {
      if (this.queues.get(sessionId) === operation) this.queues.delete(sessionId);
    }
  }

  private emit(event: SessionSaveEvent): void {
    for (const listener of this.listeners) listener(event);
  }
}

export type SessionFontSizeState = string | "mixed";

export function resolveFontSizeState(
  explicitSizes: Iterable<string | null | undefined>,
  defaultSize = "16px",
): SessionFontSizeState {
  const sizes = new Set<string>();
  for (const size of explicitSizes) sizes.add(size || defaultSize);
  if (sizes.size > 1) return "mixed";
  return sizes.values().next().value ?? defaultSize;
}

export function normalizeSessionHtml(content: string): string {
  return content.trim();
}

export function shouldLoadSessionContent(input: {
  currentHtml: string;
  incomingHtml: string;
  currentSessionId: string | null;
  incomingSessionId: string;
}): boolean {
  return (
    input.currentSessionId !== input.incomingSessionId
    || normalizeSessionHtml(input.currentHtml) !== normalizeSessionHtml(input.incomingHtml)
  );
}

export interface EditorSelectionSnapshot {
  from: number;
  to: number;
  documentGeneration: number;
}

export function isSelectionSnapshotValid(
  snapshot: EditorSelectionSnapshot,
  documentGeneration: number,
  documentSize: number,
): boolean {
  return (
    snapshot.documentGeneration === documentGeneration
    && snapshot.from >= 0
    && snapshot.to >= snapshot.from
    && snapshot.to <= documentSize
  );
}
