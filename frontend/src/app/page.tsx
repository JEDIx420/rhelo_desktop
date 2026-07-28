"use client";

import { Component, useState, useEffect, useRef, type ErrorInfo, type ReactNode } from "react";
import Sidebar from "@/components/Sidebar";
import CommandCenter from "@/components/CommandCenter";
import AppViewRouter from "@/components/AppViewRouter";
import TranscriptReviewModal from "@/components/TranscriptReviewModal";
import { TtsWarning } from "@/components/TtsWarning";
import { motion, AnimatePresence } from "framer-motion";
import { AlertTriangle, Mic, Save, X } from "lucide-react";
import { fetchDatabaseStartupStatus, fetchSessions, updateSession } from "@/lib/api";
import { readVerseDragPayload, renderVerseDropHtml, VerseDragPayload } from "@/lib/verseDrop";
import { createTtsSettingsTarget, type OriginalLanguage, type TtsSettingsTarget } from "@/lib/ttsRecovery";
import { flushActiveSessionEdits } from "@/lib/sessionSaveBridge";
import { buildTranscriptAppendHtml } from "@/lib/transcriptReview";

const invokeTauri = async (cmd: string, args: any) => {
  if (typeof window !== "undefined" && (window as any).__TAURI_INTERNALS__ !== undefined) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke(cmd, args);
  }
  throw new Error("Tauri IPC bridge not available in this environment");
};

const resampleTo16k = (audioBuffer: AudioBuffer): number[] => {
  const inputSampleRate = audioBuffer.sampleRate;
  const targetSampleRate = 16000;
  const inputBuffer = audioBuffer.getChannelData(0); // mono
  
  if (inputSampleRate === targetSampleRate) {
    return Array.from(inputBuffer);
  }
  
  const ratio = inputSampleRate / targetSampleRate;
  const newLength = Math.round(inputBuffer.length / ratio);
  const result = new Float32Array(newLength);
  
  for (let i = 0; i < newLength; i++) {
    const nextIndex = Math.min(inputBuffer.length - 1, Math.round(i * ratio));
    result[i] = inputBuffer[nextIndex];
  }
  
  return Array.from(result);
};

const addDateHeaderIfNeeded = (currentContent: string) => {
  const today = new Date();
  const dateString = today.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
  const cleanContent = currentContent ? currentContent.trim() : "";
  if (!cleanContent.includes(dateString)) {
    const heading = `<h3 style="color: #2563eb; margin-top: 20px; margin-bottom: 8px; border-bottom: 1px solid #e2e8f0; padding-bottom: 4px;">${dateString}</h3>`;
    if (cleanContent === "" || cleanContent === "<p></p>" || cleanContent === "<h3></h3>") {
      return heading;
    } else {
      return cleanContent + heading;
    }
  }
  return currentContent;
};

class DiagnosticErrorBoundary extends Component<
  { children: ReactNode },
  { error: Error | null }
> {
  state = { error: null as Error | null };

  static getDerivedStateFromError(error: Error) {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    console.error("[Rhelo diagnostic] Main panel render failed", error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <div className="m-6 rounded-2xl border border-red-200 bg-red-50 p-6 text-red-950">
          <h2 className="text-lg font-bold">DIAGNOSTIC TRAP: Main panel crashed.</h2>
          <pre className="mt-4 whitespace-pre-wrap break-words text-xs leading-5">
            {this.state.error.stack || this.state.error.message}
          </pre>
        </div>
      );
    }
    return this.props.children;
  }
}

export default function Home() {
  const [activeView, setActiveView] = useState("read");
  const [book, setBook] = useState("GEN");
  const [chapter, setChapter] = useState(1);
  const [selectedVerseId, setSelectedVerseId] = useState<string | null>(null);
  const [selectedPersonId, setSelectedPersonId] = useState<string | null>("Adam_1");
  const [settingsTarget, setSettingsTarget] = useState<TtsSettingsTarget | null>(null);
  const [sessionSaveError, setSessionSaveError] = useState<{
    message: string;
    targetView: string;
  } | null>(null);
  const [contentUpdateWarning, setContentUpdateWarning] = useState<string | null>(null);

  // Drag-and-drop overlays state
  const [draggedVerse, setDraggedVerse] = useState<VerseDragPayload | null>(null);

  // Active session states
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);
  const [activeSessionTitle, setActiveSessionTitle] = useState<string | null>(null);
  const [studySessionsList, setStudySessionsList] = useState<any[]>([]);

  // STT Voice recording state
  const [isRecording, setIsRecording] = useState(false);
  const mediaRecorderRef = useRef<MediaRecorder | null>(null);
  const audioChunksRef = useRef<Blob[]>([]);

  // STT Dictation review modal states
  const [transcribedText, setTranscribedText] = useState<string | null>(null);
  const [isProcessingSTT, setIsProcessingSTT] = useState(false);
  const [reviewTargetSessionId, setReviewTargetSessionId] = useState<string | null>(null);

  useEffect(() => {
    if (typeof window === "undefined" || !(window as any).__TAURI_INTERNALS__) return;
    void fetchDatabaseStartupStatus()
      .then((status) => setContentUpdateWarning(status.warning))
      .catch(() => undefined);
  }, []);

  // Track window drag listeners
  // WebKit (WKWebView/Tauri macOS) fires dragend before drop completes —
  // debounce the state clear so the drop target stays mounted long enough
  // for onDrop to fire and process.
  useEffect(() => {
    let dragEndTimer: ReturnType<typeof setTimeout> | null = null;

    const handleDragStartEvent = (e: Event) => {
      // If a new drag starts while a previous dragend timer is pending, cancel it
      if (dragEndTimer) { clearTimeout(dragEndTimer); dragEndTimer = null; }
      const customEvent = e as CustomEvent;
      if (customEvent.detail) {
        setDraggedVerse(customEvent.detail.payload || {
          verseId: customEvent.detail.verseId,
          translations: [{ label: "Reference", text: customEvent.detail.verseText }],
        });
      }
    };

    const handleDragEndEvent = () => {
      // Debounce: give onDrop 150ms to fire before hiding the drop zone
      dragEndTimer = setTimeout(() => setDraggedVerse(null), 150);
    };

    window.addEventListener("rhelo-drag-start", handleDragStartEvent);
    window.addEventListener("rhelo-drag-end", handleDragEndEvent);

    return () => {
      window.removeEventListener("rhelo-drag-start", handleDragStartEvent);
      window.removeEventListener("rhelo-drag-end", handleDragEndEvent);
      if (dragEndTimer) clearTimeout(dragEndTimer);
    };
  }, []);

  // Sync active session selection globally
  useEffect(() => {
    const handleActiveSessionChanged = (e: Event) => {
      const customEvent = e as CustomEvent;
      if (customEvent.detail) {
        setActiveSessionId(customEvent.detail.sessionId);
        setActiveSessionTitle(customEvent.detail.title);
      }
    };
    window.addEventListener("rhelo-active-session-changed", handleActiveSessionChanged);
    return () => window.removeEventListener("rhelo-active-session-changed", handleActiveSessionChanged);
  }, []);

  // Fetch initial active session list and selection
  useEffect(() => {
    const initActiveSession = async () => {
      try {
        const res = await fetchSessions();
        const list = res.sessions || [];
        setStudySessionsList(list);
        if (list.length > 0 && !activeSessionId) {
          setActiveSessionId(list[0].session_id);
          setActiveSessionTitle(list[0].title);
        }
      } catch (err) {
        console.error(err);
      }
    };
    initActiveSession();
    
    const handleSessionUpdated = () => {
      initActiveSession();
    };
    window.addEventListener("rhelo-session-updated", handleSessionUpdated);
    return () => window.removeEventListener("rhelo-session-updated", handleSessionUpdated);
  }, [activeSessionId]);

  const resolveSessionTargetId = async () => {
    const response = await fetchSessions();
    const sessions = response.sessions || [];
    setStudySessionsList(sessions);
    const target = sessions.find((session: any) => session.session_id === activeSessionId) || sessions[0];
    if (!target) throw new Error("Create a study session before saving notes or dictation.");
    setActiveSessionId(target.session_id);
    setActiveSessionTitle(target.title);
    return target.session_id as string;
  };

  const startRecording = async () => {
    const isTauri = typeof window !== "undefined" && (window as any).__TAURI_INTERNALS__ !== undefined;
    
    if (isTauri) {
      try {
        const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
        const mediaRecorder = new MediaRecorder(stream);
        mediaRecorderRef.current = mediaRecorder;
        audioChunksRef.current = [];

        mediaRecorder.ondataavailable = (event) => {
          if (event.data.size > 0) {
            audioChunksRef.current.push(event.data);
          }
        };

        mediaRecorder.onstop = async () => {
          setIsProcessingSTT(true);
          try {
            if (audioChunksRef.current.length === 0) {
              throw new Error("The microphone did not return any audio data.");
            }
            const audioBlob = new Blob(audioChunksRef.current, {
              type: mediaRecorder.mimeType || audioChunksRef.current[0].type,
            });
            const arrayBuffer = await audioBlob.arrayBuffer();
            
            // Decode audio binary to float PCM samples via AudioContext
            const audioContext = new (window.AudioContext || (window as any).webkitAudioContext)();
            const audioBuffer = await audioContext.decodeAudioData(arrayBuffer);
            
            // Downsample float samples to 16000Hz mono
            const samples = resampleTo16k(audioBuffer);
            
            // Call offline native Tauri command
            const text = await invokeTauri("transcribe_audio", { audioSamples: samples }) as string;
            await audioContext.close();
            
            if (text && text.trim()) {
              setTranscribedText(text);
              setReviewTargetSessionId(await resolveSessionTargetId());
            } else {
              alert("Speech recognition was unable to capture any words. Please try again.");
            }
          } catch (err) {
            console.error("STT transcribing error", err);
            alert("Transcription failed: " + (err instanceof Error ? err.message : String(err)));
          } finally {
            setIsProcessingSTT(false);
          }
          stream.getTracks().forEach((track) => track.stop());
        };

        mediaRecorder.start();
        setIsRecording(true);
      } catch (err) {
        console.error("Microphone access denied", err);
        alert("Microphone access denied or not supported: " + (err instanceof Error ? err.message : String(err)));
      }
    } else {
      // Fallback: Web browser native SpeechRecognition (Chrome, Safari, etc.)
      const SpeechRecognition = (window as any).SpeechRecognition || (window as any).webkitSpeechRecognition;
      if (!SpeechRecognition) {
        alert("Speech recognition is not supported in this browser. Please use Chrome or run inside the Tauri desktop application.");
        return;
      }
      
      try {
        const recognition = new SpeechRecognition();
        recognition.continuous = false;
        recognition.interimResults = false;
        recognition.lang = "en-US";
        
        recognition.onstart = () => {
          setIsRecording(true);
        };
        
        recognition.onerror = (event: any) => {
          console.error("Speech recognition error", event.error);
          setIsRecording(false);
          alert("Speech recognition error: " + event.error);
        };
        
        recognition.onend = () => {
          setIsRecording(false);
        };
        
        recognition.onresult = async (event: any) => {
          const text = event.results[0][0].transcript;
          if (text && text.trim()) {
            setTranscribedText(text);
            try {
              setReviewTargetSessionId(await resolveSessionTargetId());
            } catch (error) {
              setTranscribedText(null);
              alert(error instanceof Error ? error.message : String(error));
            }
          } else {
            alert("Speech recognition was unable to capture any words. Please try again.");
          }
        };
        
        (window as any)._activeRecognition = recognition;
        recognition.start();
      } catch (err) {
        console.error("Failed to start SpeechRecognition", err);
        setIsRecording(false);
      }
    }
  };

  const stopRecording = () => {
    const isTauri = typeof window !== "undefined" && (window as any).__TAURI_INTERNALS__ !== undefined;
    
    if (isTauri) {
      if (mediaRecorderRef.current && isRecording) {
        mediaRecorderRef.current.stop();
        setIsRecording(false);
      }
    } else {
      const recognition = (window as any)._activeRecognition;
      if (recognition) {
        recognition.stop();
      }
      setIsRecording(false);
    }
  };

  const handleConfirmTranscription = async () => {
    if (!reviewTargetSessionId || !transcribedText || !transcribedText.trim()) return;
    try {
      await flushActiveSessionEdits();
      const sessionsResponse = await fetchSessions();
      const sessions = sessionsResponse.sessions || [];
      const targetSession = sessions.find((session: any) => session.session_id === reviewTargetSessionId);
      if (!targetSession) throw new Error("The selected study session no longer exists.");
      
      const contentWithDate = addDateHeaderIfNeeded(targetSession.content || "");
      const timestamp = new Date().toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: true });
      
      const updatedContent = contentWithDate + buildTranscriptAppendHtml(transcribedText, timestamp);
      await updateSession(reviewTargetSessionId, targetSession.title, updatedContent);
      setStudySessionsList(sessions.map((session: any) =>
        session.session_id === reviewTargetSessionId
          ? { ...session, content: updatedContent, updated_at: new Date().toISOString() }
          : session
      ));
      
      window.dispatchEvent(new CustomEvent("rhelo-session-updated"));
      window.dispatchEvent(new CustomEvent("rhelo-active-session-changed", {
        detail: { sessionId: reviewTargetSessionId, title: targetSession.title }
      }));
      
      setTranscribedText(null);
      handleViewChange("sessions");
    } catch (err) {
      console.error(err);
      alert(`The transcription could not be saved: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handleNavigate = (b: string, c: number, v?: number) => {
    setBook(b);
    setChapter(c);
    if (v) {
      setSelectedVerseId(`${b}.${c}.${v}`);
    } else {
      setSelectedVerseId(null);
    }
  };

  const handleViewChange = async (view: string) => {
    if (activeView === "sessions" && view !== "sessions") {
      try {
        await flushActiveSessionEdits();
        setSessionSaveError(null);
      } catch (error) {
        setSessionSaveError({
          message: `Study Session changes could not be saved before navigation: ${
            error instanceof Error ? error.message : String(error)
          }`,
          targetView: view,
        });
        return;
      }
    }
    if (view !== "settings") setSettingsTarget(null);
    setActiveView(view);
  };

  const handleGoToTtsSettings = (missingLanguage: OriginalLanguage) => {
    setSettingsTarget(createTtsSettingsTarget(missingLanguage, Date.now()));
    void handleViewChange("settings");
  };

  return (
    <div className="flex h-screen w-screen overflow-hidden bg-slate-50 relative print:!block print:!h-auto print:!min-h-0 print:!overflow-visible">
      {/* CommandCenter Keyboard-Activated Command Launcher */}
      <CommandCenter
        onNavigate={handleNavigate}
        onSelectPerson={setSelectedPersonId}
        onViewChange={handleViewChange}
      />

      {/* Persistent Sidebar */}
      <Sidebar activeView={activeView} onViewChange={handleViewChange} />
      
      {/* Main Panel Viewport */}
      <main className="flex-1 overflow-hidden print:!block print:!h-auto print:!min-h-0 print:!overflow-visible">
        <AnimatePresence mode="wait">
          <motion.div
            key={activeView}
            initial={{ opacity: 0, y: 12 }}
            animate={{ opacity: 1, y: 0 }}
            exit={{ opacity: 0, y: -12 }}
            transition={{ duration: 0.2 }}
            className="h-full"
          >
            <DiagnosticErrorBoundary key={activeView}>
              <AppViewRouter
                activeView={activeView}
                book={book}
                chapter={chapter}
                selectedVerseId={selectedVerseId}
                selectedPersonId={selectedPersonId}
                setBook={setBook}
                setChapter={setChapter}
                setSelectedVerseId={setSelectedVerseId}
                setSelectedPersonId={setSelectedPersonId}
                setActiveView={handleViewChange}
                settingsTarget={settingsTarget}
                onNavigate={handleNavigate}
              />
            </DiagnosticErrorBoundary>
          </motion.div>
        </AnimatePresence>
      </main>

      <TtsWarning onGoToSettings={handleGoToTtsSettings} />

      {contentUpdateWarning ? (
        <div
          role="status"
          className="fixed left-1/2 top-5 z-[2600] flex w-[min(680px,calc(100%-2rem))] -translate-x-1/2 items-start gap-3 rounded-xl border border-amber-300 bg-amber-50 px-4 py-3 text-sm text-amber-950 shadow-xl"
        >
          <AlertTriangle size={18} className="mt-0.5 shrink-0" />
          <span className="flex-1">{contentUpdateWarning}</span>
          <button
            type="button"
            onClick={() => setContentUpdateWarning(null)}
            aria-label="Dismiss content update warning"
            className="rounded-md p-1 hover:bg-amber-100"
          >
            <X size={16} />
          </button>
        </div>
      ) : null}

      {sessionSaveError ? (
        <div className="fixed bottom-6 left-1/2 z-[2500] flex max-w-2xl -translate-x-1/2 items-center gap-3 rounded-xl border border-amber-300 bg-amber-50 px-4 py-3 text-sm text-amber-950 shadow-xl">
          <span>{sessionSaveError.message} Your captured changes remain queued while this editor stays open.</span>
          <button
            type="button"
            onClick={() => void handleViewChange(sessionSaveError.targetView)}
            className="shrink-0 rounded-md bg-amber-100 px-2 py-1 font-semibold hover:bg-amber-200"
          >
            Retry
          </button>
          <button
            type="button"
            onClick={() => {
              const targetView = sessionSaveError.targetView;
              setSessionSaveError(null);
              if (targetView !== "settings") setSettingsTarget(null);
              setActiveView(targetView);
            }}
            className="shrink-0 rounded-md px-2 py-1 font-semibold hover:bg-amber-100"
          >
            Leave without saving
          </button>
        </div>
      ) : null}

      {/* --- FLOATING WORKSPACE OVERLAYS --- */}

      {/* 1. Listening Waveform Pill (STT Dictation Mode) */}
      <AnimatePresence>
        {(isRecording || isProcessingSTT) && (
          <motion.div
            initial={{ opacity: 0, y: -20, scale: 0.9 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: -20, scale: 0.9 }}
            className="fixed top-6 right-6 z-[2000] px-4 py-2.5 bg-slate-900/95 backdrop-blur-md border border-slate-800 rounded-full flex items-center gap-3 shadow-xl text-white font-sans"
          >
            {isRecording ? (
              <>
                <div className="flex gap-1 items-center h-4">
                  <span className="w-0.5 bg-blue-400 h-2 animate-bounce rounded" style={{ animationDelay: '0.1s' }} />
                  <span className="w-0.5 bg-blue-400 h-4 animate-bounce rounded" style={{ animationDelay: '0.2s' }} />
                  <span className="w-0.5 bg-blue-400 h-1 animate-bounce rounded" style={{ animationDelay: '0.3s' }} />
                  <span className="w-0.5 bg-blue-400 h-3 animate-bounce rounded" style={{ animationDelay: '0.4s' }} />
                </div>
                <span className="text-xs font-bold pr-1">Speech Dictation Active...</span>
              </>
            ) : (
              <>
                <div className="w-3.5 h-3.5 rounded-full border-2 border-white/30 border-t-white animate-spin" />
                <span className="text-xs font-bold pr-1">Transcribing Speech...</span>
              </>
            )}
          </motion.div>
        )}
      </AnimatePresence>

      {/* 2. Magnetic Drop Zone overlay — always mounted, visibility toggled via CSS
         to prevent WebKit's dragend from unmounting the target before onDrop fires */}
      <motion.div
        initial={{ opacity: 0, scale: 0.8, y: 50 }}
        animate={
          draggedVerse
            ? { opacity: 1, scale: 1, y: 0, pointerEvents: "auto" as const }
            : { opacity: 0, scale: 0.8, y: 50, pointerEvents: "none" as const }
        }
        transition={{ type: "spring", stiffness: 400, damping: 30 }}
        className="fixed bottom-6 right-6 z-[2000] p-5 rounded-2xl border-2 border-dashed border-blue-400 bg-white/95 backdrop-blur-md shadow-2xl flex flex-col items-center justify-center gap-2"
        onDragEnter={(e) => { e.preventDefault(); e.stopPropagation(); console.log("[DRAG-TRACE] onDragEnter fired!"); }}
        onDragOver={(e) => { e.preventDefault(); e.stopPropagation(); }}
        onDragLeave={() => console.log("[DRAG-TRACE] onDragLeave fired!")}
        onDrop={async (e) => {
          e.preventDefault();
          e.stopPropagation();
          console.log("[DRAG-TRACE] onDrop fired!");
          const versePayload = readVerseDragPayload(e.dataTransfer) || draggedVerse;
          console.log("[DRAG-DEBUG] e.dataTransfer types:", e.dataTransfer?.types);
          console.log("[DRAG-DEBUG] Result of readVerseDragPayload:", readVerseDragPayload(e.dataTransfer));
          console.log("[DRAG-DEBUG] Current draggedVerse state:", draggedVerse);
          if (versePayload) {
            try {
              await flushActiveSessionEdits();
              const sessionsResponse = await fetchSessions();
              const sessions = sessionsResponse.sessions || [];
              const targetSession = sessions.find((session: any) => session.session_id === activeSessionId) || sessions[0];
              if (!targetSession) throw new Error("Create a study session before dropping a reference.");

              const contentWithDate = addDateHeaderIfNeeded(targetSession.content || "");
              const updatedContent = contentWithDate + renderVerseDropHtml(versePayload);
              await updateSession(targetSession.session_id, targetSession.title, updatedContent);
              setStudySessionsList(sessions.map((session: any) =>
                session.session_id === targetSession.session_id
                  ? { ...session, content: updatedContent, updated_at: new Date().toISOString() }
                  : session
              ));

              window.dispatchEvent(new CustomEvent("rhelo-session-updated"));
              handleViewChange("sessions");
            } catch (err) {
              const msg = `[RHELO-DROP-DEBUG] ${err instanceof Error ? err.stack || err.message : String(err)}`;
              console.error(msg);
            }
          }
          setDraggedVerse(null);
        }}
      >
        {/* All children are pointer-events-none so the parent captures all drag events */}
        <div className="pointer-events-none flex flex-col items-center justify-center gap-2">
          <div className="w-12 h-12 rounded-full bg-blue-50 border border-blue-200 flex items-center justify-center text-blue-600 animate-bounce">
            <Save size={20} />
          </div>
          <p className="text-sm font-bold text-slate-800 font-sans">Drop here to save reference</p>
          <p className="text-[11px] text-slate-400 font-sans">Appends to: <strong className="text-blue-600">{activeSessionTitle || "latest session"}</strong></p>
        </div>
      </motion.div>

      {/* 3. Floating Voice Dictation mic toggle */}
      <div 
        className="fixed bottom-6 z-[1900] flex items-center gap-2 group/mic"
        style={{ right: draggedVerse ? "260px" : "24px" }}
      >
        <span className="opacity-0 group-hover/mic:opacity-100 transition-opacity bg-slate-900/90 text-white text-[11px] font-bold px-3 py-1.5 rounded-xl shadow-md pointer-events-none uppercase tracking-wider font-sans whitespace-nowrap">
          {isRecording ? "Recording... Click to Stop" : "Voice Dictation"}
        </span>
        <button
          onClick={isRecording ? stopRecording : startRecording}
          className={`p-4.5 rounded-full shadow-2xl transition-all hover:scale-105 active:scale-95 cursor-pointer flex items-center justify-center border border-white/20 ${
            isRecording 
              ? "bg-red-600 text-white animate-pulse" 
              : "bg-blue-600 text-white hover:bg-blue-700"
          }`}
          title={isRecording ? "Stop dictation" : "Start speech dictation"}
        >
          <Mic size={22} />
        </button>
      </div>

      {transcribedText !== null ? (
        <TranscriptReviewModal
          transcript={transcribedText}
          sessions={studySessionsList}
          targetSessionId={reviewTargetSessionId}
          onTranscriptChange={setTranscribedText}
          onTargetSessionChange={setReviewTargetSessionId}
          onDiscard={() => setTranscribedText(null)}
          onAdd={() => void handleConfirmTranscription()}
        />
      ) : null}
    </div>
  );
}
