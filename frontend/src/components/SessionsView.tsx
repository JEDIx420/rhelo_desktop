"use client";

import { useState, useEffect, useRef } from "react";
import { useEditor, useEditorState, EditorContent } from "@tiptap/react";
import StarterKit from "@tiptap/starter-kit";
import { Mark } from "@tiptap/core";
import { EditorState } from "@tiptap/pm/state";
import { TextStyle } from "@tiptap/extension-text-style";
import { TextAlign } from "@tiptap/extension-text-align";
import { 
  Notebook, 
  Plus, 
  Search, 
  Trash2, 
  FileDown, 
  Save, 
  Check, 
  Loader2,
  Calendar,
  Undo2,
  Redo2,
  Bold,
  Italic,
  Underline as UnderlineIcon,
  Strikethrough,
  AlignLeft,
  AlignCenter,
  AlignRight,
  AlignJustify,
  List,
  ListOrdered,
  Quote
} from "lucide-react";
import { 
  fetchSessions, 
  createSession, 
  updateSession, 
  deleteSession, 
  searchSessions, 
} from "@/lib/api";
import { readVerseDragPayload, renderVerseDropHtml } from "@/lib/verseDrop";
import {
  isSelectionSnapshotValid,
  resolveFontSizeState,
  SessionSaveCoordinator,
  shouldLoadSessionContent,
  type EditorSelectionSnapshot,
} from "@/lib/sessionEditor";
import { registerSessionSaveFlusher } from "@/lib/sessionSaveBridge";

interface Session {
  session_id: string;
  title: string;
  content: string;
  updated_at: string;
}

const FontSize = Mark.create({
  name: 'fontSize',
  addAttributes() {
    return {
      size: {
        default: null,
        parseHTML: element => element.style.fontSize,
        renderHTML: attributes => {
          if (!attributes.size) {
            return {}
          }
          return { style: `font-size: ${attributes.size}` }
        },
      },
    }
  },
  parseHTML() {
    return [
      {
        tag: 'span[style*=font-size]',
      },
    ]
  },
  renderHTML({ HTMLAttributes }) {
    return ['span', HTMLAttributes, 0]
  }
});

export default function SessionsView() {
  const [sessions, setSessions] = useState<Session[]>([]);
  const [selectedSession, setSelectedSession] = useState<Session | null>(null);
  const [searchQuery, setSearchQuery] = useState("");
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [exporting, setExporting] = useState(false);
  const [showSuccessFlash, setShowSuccessFlash] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [titleInput, setTitleInput] = useState("");
  const titleInputRef = useRef(titleInput);
  const selectedSessionRef = useRef<Session | null>(null);
  const successFlashTimeoutRef = useRef<NodeJS.Timeout | null>(null);
  const mountedRef = useRef(true);
  const loadedSessionIdRef = useRef<string | null>(null);
  const synchronizingRef = useRef(false);
  const documentGenerationRef = useRef(0);
  const selectionSnapshotRef = useRef<EditorSelectionSnapshot | null>(null);
  const [saveCoordinator] = useState(() => (
    new SessionSaveCoordinator({
      persist: async (snapshot) => {
        await updateSession(snapshot.sessionId, snapshot.title, snapshot.content);
      },
    })
  ));

  const scheduleSessionSave = (htmlContent: string, flush = false) => {
    const target = selectedSessionRef.current;
    if (!target) return Promise.resolve();
    saveCoordinator.schedule({
      sessionId: target.session_id,
      title: titleInputRef.current,
      content: htmlContent,
    });
    return flush
      ? saveCoordinator.flush(target.session_id)
      : Promise.resolve();
  };

  // Initialize TipTap
  const editor = useEditor({
    immediatelyRender: true,
    extensions: [
      StarterKit,
      TextAlign.configure({
        types: ['heading', 'paragraph'],
      }),
      TextStyle,
      FontSize,
    ],
    content: "",
    onUpdate: ({ editor }) => {
      if (synchronizingRef.current) return;
      documentGenerationRef.current += 1;
      selectionSnapshotRef.current = null;
      // Trigger auto-save after 1.5 seconds of inactivity
      void scheduleSessionSave(editor.getHTML());
    },
    onFocus: ({ editor }) => {
      const today = new Date();
      const dateString = today.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
      const html = editor.getHTML();
      if (!html.includes(dateString)) {
        const heading = `<h3 style="color: #2563eb; margin-top: 20px; margin-bottom: 8px; border-bottom: 1px solid #e2e8f0; padding-bottom: 4px;">${dateString}</h3>`;
        editor.commands.insertContentAt(editor.state.doc.content.size, heading);
      }
    },
    editorProps: {
      attributes: {
        class: "prose prose-slate focus:outline-none max-w-none h-full min-h-[400px] text-slate-800 leading-relaxed font-sans px-2",
      },
    },
  });

  const toolbarState = useEditorState({
    editor,
    selector: ({ editor: currentEditor }) => {
      if (!currentEditor) {
        return {
          bold: false,
          italic: false,
          underline: false,
          strike: false,
          bulletList: false,
          orderedList: false,
          blockquote: false,
          heading: "p",
          textAlign: "left",
          canUndo: false,
          canRedo: false,
          fontSize: "16px" as string,
        };
      }

      const { from, to, empty } = currentEditor.state.selection;
      const explicitSizes: Array<string | null> = [];
      if (empty) {
        explicitSizes.push(currentEditor.getAttributes("fontSize").size ?? null);
      } else {
        currentEditor.state.doc.nodesBetween(from, to, (node, position) => {
          if (!node.isText) return;
          const nodeEnd = position + node.nodeSize;
          if (nodeEnd <= from || position >= to) return;
          const fontSizeMark = node.marks.find((mark) => mark.type.name === "fontSize");
          explicitSizes.push(fontSizeMark?.attrs.size ?? null);
        });
      }

      return {
        bold: currentEditor.isActive("bold"),
        italic: currentEditor.isActive("italic"),
        underline: currentEditor.isActive("underline"),
        strike: currentEditor.isActive("strike"),
        bulletList: currentEditor.isActive("bulletList"),
        orderedList: currentEditor.isActive("orderedList"),
        blockquote: currentEditor.isActive("blockquote"),
        heading: currentEditor.isActive("heading", { level: 1 }) ? "h1"
          : currentEditor.isActive("heading", { level: 2 }) ? "h2"
          : currentEditor.isActive("heading", { level: 3 }) ? "h3"
          : "p",
        textAlign: currentEditor.getAttributes("paragraph").textAlign
          || currentEditor.getAttributes("heading").textAlign
          || "left",
        canUndo: currentEditor.can().undo(),
        canRedo: currentEditor.can().redo(),
        fontSize: resolveFontSizeState(explicitSizes),
      };
    },
  });

  const loadSessions = async () => {
    Promise.resolve().then(() => {
      setLoading(true);
    });
    try {
      const data = await fetchSessions();
      const sList = data.sessions || [];
      setSessions(sList);
      setSelectedSession((current) => {
        if (sList.length === 0) return null;
        if (!current) return sList[0];
        return sList.find((session: Session) => session.session_id === current.session_id) || sList[0];
      });
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  // Fetch sessions on mount
  useEffect(() => {
    Promise.resolve().then(() => {
      loadSessions();
    });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Refresh the active editor when another workflow updates a session (for example, STT).
  useEffect(() => {
    const handleSessionUpdated = () => {
      void loadSessions();
    };

    window.addEventListener("rhelo-session-updated", handleSessionUpdated);
    return () => window.removeEventListener("rhelo-session-updated", handleSessionUpdated);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // Keep ref in sync with titleInput state
  useEffect(() => {
    titleInputRef.current = titleInput;
  }, [titleInput]);

  useEffect(() => {
    selectedSessionRef.current = selectedSession;
  }, [selectedSession]);

  useEffect(() => {
    mountedRef.current = true;
    const unsubscribe = saveCoordinator.subscribe((event) => {
      if (!mountedRef.current) return;
      setSaving(false);
      if (event.type === "error") {
        setSaveError(`Your latest changes are still unsaved: ${event.error.message}`);
        return;
      }

      setSaveError(null);
      const { snapshot } = event;
      const updatedAt = new Date().toISOString();
      setSessions((current) => current.map((session) => (
        session.session_id === snapshot.sessionId
          ? {
              ...session,
              title: snapshot.title,
              content: snapshot.content,
              updated_at: updatedAt,
            }
          : session
      )));
      if (selectedSessionRef.current?.session_id === snapshot.sessionId) {
        selectedSessionRef.current = {
          ...selectedSessionRef.current,
          title: snapshot.title,
          content: snapshot.content,
          updated_at: updatedAt,
        };
      }
    });
    return () => {
      mountedRef.current = false;
      unsubscribe();
      void saveCoordinator.flushAll().finally(() => saveCoordinator.dispose());
      if (successFlashTimeoutRef.current) {
        clearTimeout(successFlashTimeoutRef.current);
      }
    };
  }, [saveCoordinator]);

  useEffect(() => registerSessionSaveFlusher(async () => {
    await saveCoordinator.flushAll();
  }), [saveCoordinator]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;

    void import("@tauri-apps/api/window").then(async ({ getCurrentWindow }) => {
      if (cancelled) return;
      const appWindow = getCurrentWindow();
      unlisten = await appWindow.onCloseRequested(async (event) => {
        event.preventDefault();
        try {
          await saveCoordinator.flushAll();
          await appWindow.destroy();
        } catch {
          // The coordinator already exposes a persistent error banner.
        }
      });
      if (cancelled) unlisten();
    }).catch(() => {
      // Browser development mode does not expose a Tauri window.
    });

    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [saveCoordinator]);

  // Dispatch selection changes globally
  useEffect(() => {
    if (selectedSession) {
      window.dispatchEvent(new CustomEvent("rhelo-active-session-changed", {
        detail: { sessionId: selectedSession.session_id, title: selectedSession.title }
      }));
    }
  }, [selectedSession]);

  // Synchronize selection with external updates (e.g. from StudyPane)
  useEffect(() => {
    const handleActiveSessionChanged = (e: Event) => {
      const customEvent = e as CustomEvent;
      if (customEvent.detail) {
        const sid = customEvent.detail.sessionId;
        if (selectedSession?.session_id !== sid) {
          const match = sessions.find((s) => s.session_id === sid);
          if (match) {
            setSelectedSession(match);
          }
        }
      }
    };
    window.addEventListener("rhelo-active-session-changed", handleActiveSessionChanged);
    return () => window.removeEventListener("rhelo-active-session-changed", handleActiveSessionChanged);
  }, [sessions, selectedSession]);

  // Update editor content when active session changes
  useEffect(() => {
    if (selectedSession && editor) {
      const incomingContent = selectedSession.content || "";
      if (shouldLoadSessionContent({
        currentHtml: editor.getHTML(),
        incomingHtml: incomingContent,
        currentSessionId: loadedSessionIdRef.current,
        incomingSessionId: selectedSession.session_id,
      })) {
        synchronizingRef.current = true;
        editor.commands.setContent(incomingContent, { emitUpdate: false });
        editor.view.updateState(EditorState.create({
          schema: editor.schema,
          doc: editor.state.doc,
          plugins: editor.state.plugins,
        }));
        loadedSessionIdRef.current = selectedSession.session_id;
        documentGenerationRef.current += 1;
        selectionSnapshotRef.current = null;
        synchronizingRef.current = false;
      }
      Promise.resolve().then(() => {
        setTitleInput(selectedSession.title);
        titleInputRef.current = selectedSession.title;
      });
    }
  }, [selectedSession, editor]);

  // Handle local session search
  useEffect(() => {
    const delayDebounceFn = setTimeout(() => {
      if (searchQuery.trim()) {
        setLoading(true);
        searchSessions(searchQuery)
          .then((data) => setSessions(data.sessions || []))
          .catch(console.error)
          .finally(() => setLoading(false));
      } else {
        loadSessions();
      }
    }, 300);

    return () => clearTimeout(delayDebounceFn);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [searchQuery]);

  const handleCreateSession = async () => {
    try {
      await saveCoordinator.flushAll();
      setLoading(true);
      const today = new Date();
      const dateString = today.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
      const initialContent = `<h3 style="color: #2563eb; margin-top: 20px; margin-bottom: 8px; border-bottom: 1px solid #e2e8f0; padding-bottom: 4px;">${dateString}</h3><p></p>`;
      const res = await createSession("Untitled Study Session", initialContent);
      const refreshed = await fetchSessions();
      const refreshedSessions = refreshed.sessions || [];
      setSessions(refreshedSessions);
      setSelectedSession(
        refreshedSessions.find((session: Session) => session.session_id === res.session_id)
          || refreshedSessions[0]
          || null,
      );
    } catch (err) {
      console.error(err);
    } finally {
      setLoading(false);
    }
  };

  const handleManualSave = async () => {
    if (!selectedSession || !editor) return;
    setSaving(true);
    try {
      await scheduleSessionSave(editor.getHTML(), true);
      
      // Flash save state
      setTimeout(() => setSaving(false), 500);
    } catch (err) {
      console.error(err);
      setSaving(false);
    }
  };

  const handleSelectSession = async (session: Session) => {
    if (session.session_id === selectedSessionRef.current?.session_id) return;
    try {
      await saveCoordinator.flushAll();
    } catch {
      // Navigation continues; the failed immutable snapshot remains retryable.
    }
    setSelectedSession(session);
  };

  const handleDeleteSession = async (id: string, e: React.MouseEvent) => {
    e.stopPropagation();
    if (!confirm("Are you sure you want to delete this study session?")) return;
    saveCoordinator.discardSession(id);
    try {
      await deleteSession(id);
      setSessions((prev) => prev.filter((s) => s.session_id !== id));
      if (selectedSession?.session_id === id) {
        setSelectedSession(null);
      }
    } catch (err) {
      saveCoordinator.restoreSession(id);
      console.error(err);
      setSaveError(`The study session could not be deleted: ${err instanceof Error ? err.message : String(err)}`);
    }
  };

  const handleExportPDF = async () => {
    if (!selectedSession || !editor) return;
    setExporting(true);
    try {
      await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      const originalTitle = document.title;
      // Fallback through the active states to get the best title
      document.title = selectedSession?.title || titleInput || 'Study_Session';
      window.print();
      // Restore title after the OS dialog captures it
      setTimeout(() => {
        document.title = originalTitle;
      }, 500);
    } catch (err) {
      console.error("PDF export failed", err);
    } finally {
      setExporting(false);
    }
  };

  const handleDropVerse = (e: React.DragEvent) => {
    e.preventDefault();
    const versePayload = readVerseDragPayload(e.dataTransfer);
    
    if (editor) {
      // Ensure date header exists
      const today = new Date();
      const dateString = today.toLocaleDateString(undefined, { year: 'numeric', month: 'long', day: 'numeric' });
      const html = editor.getHTML();
      if (!html.includes(dateString)) {
        const heading = `<h3 style="color: #2563eb; margin-top: 20px; margin-bottom: 8px; border-bottom: 1px solid #e2e8f0; padding-bottom: 4px;">${dateString}</h3>`;
        editor.commands.insertContentAt(editor.state.doc.content.size, heading);
      }

      if (versePayload) {
        editor.commands.insertContent(renderVerseDropHtml(versePayload));
        void scheduleSessionSave(editor.getHTML());
      }
    }
  };

  const captureSelection = () => {
    if (!editor) return;
    selectionSnapshotRef.current = {
      from: editor.state.selection.from,
      to: editor.state.selection.to,
      documentGeneration: documentGenerationRef.current,
    };
  };

  const restoreCapturedSelection = () => {
    if (!editor || !selectionSnapshotRef.current) return;
    const snapshot = selectionSnapshotRef.current;
    if (!isSelectionSnapshotValid(
      snapshot,
      documentGenerationRef.current,
      editor.state.doc.content.size,
    )) {
      selectionSnapshotRef.current = null;
      return;
    }
    editor.commands.setTextSelection({ from: snapshot.from, to: snapshot.to });
  };

  const handleToolbarPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if ((event.target as HTMLElement).closest("button")) event.preventDefault();
  };

  return (
    <div className="flex h-full w-full overflow-hidden bg-slate-50 print:!block print:!h-auto print:!overflow-visible print:bg-white print-expand-shell">
      {saveError ? (
        <div className="fixed left-1/2 top-5 z-[2600] flex max-w-xl -translate-x-1/2 items-center gap-3 rounded-xl border border-amber-300 bg-amber-50 px-4 py-3 text-sm text-amber-950 shadow-xl">
          <span>{saveError}</span>
          <button
            type="button"
            onClick={() => {
              setSaving(true);
              void saveCoordinator.flushAll().catch(() => undefined);
            }}
            className="rounded-md bg-amber-100 px-2 py-1 font-semibold hover:bg-amber-200"
          >
            Retry
          </button>
        </div>
      ) : null}
      {/* Left Sidebar List Pane */}
      <div className="w-80 border-r border-slate-200 flex flex-col shrink-0 bg-white print:hidden print-hide-sidebar">
        <div className="h-16 px-5 border-b border-slate-200 flex items-center justify-between shrink-0">
          <div className="flex items-center gap-2.5">
            <Notebook size={20} className="text-blue-600" />
            <h3 className="font-bold text-base text-slate-900 font-sans">
              Study Sessions
            </h3>
          </div>
          <button
            onClick={handleCreateSession}
            className="p-1.5 rounded-lg hover:bg-slate-100 text-slate-600 hover:text-slate-900 cursor-pointer transition-colors border border-slate-200/50"
            title="Create new session"
          >
            <Plus size={18} />
          </button>
        </div>

        {/* Search filter */}
        <div className="p-3.5 border-b border-slate-100 bg-slate-50/50 shrink-0">
          <div className="relative">
            <Search className="absolute left-3 top-2.5 text-slate-400" size={16} />
            <input
              type="text"
              placeholder="Search session content..."
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              className="w-full pl-9 pr-4 py-2 text-sm border border-slate-200 rounded-xl bg-white focus:outline-none focus:ring-2 focus:ring-blue-500/20 focus:border-blue-500 transition-all font-sans"
            />
          </div>
        </div>

        {/* Sessions scrollable list */}
        <div className="flex-1 overflow-y-auto p-3 space-y-1.5">
          {loading && sessions.length === 0 ? (
            <div className="flex flex-col items-center justify-center h-32 gap-2 text-slate-500 text-sm">
              <Loader2 className="animate-spin text-blue-500" size={24} />
              <span>Loading sessions...</span>
            </div>
          ) : sessions.length === 0 ? (
            <div className="text-center py-20 px-5 text-sm text-slate-400 font-sans flex flex-col items-center justify-center gap-3">
              <Notebook size={24} className="text-slate-300" />
              <p className="font-semibold text-slate-700">No sessions found</p>
              <p className="text-xs text-slate-500 max-w-[180px] leading-relaxed">Create a session using the plus button to record your study logs.</p>
            </div>
          ) : (
            sessions.map((s) => {
              const isSelected = selectedSession?.session_id === s.session_id;
              const formattedDate = new Date(s.updated_at).toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                hour: "2-digit",
                minute: "2-digit"
              });
              return (
                <div
                  key={s.session_id}
                  onClick={() => void handleSelectSession(s)}
                  className="w-full text-left p-3.5 rounded-xl transition-all flex items-start gap-3 cursor-pointer border border-transparent hover:border-slate-200 group/item font-sans"
                  style={{
                    background: isSelected ? "rgba(37, 99, 235, 0.05)" : "transparent",
                    borderColor: isSelected ? "rgba(37, 99, 235, 0.2)" : "transparent",
                  }}
                >
                  <div className="flex-1 min-w-0">
                    <div className="font-bold text-slate-900 truncate group-hover/item:text-blue-600 transition-colors font-sans">
                      {s.title}
                    </div>
                    <div className="text-xs text-slate-400 mt-1.5 flex items-center gap-1.5 font-sans">
                      <Calendar size={11} />
                      <span>{formattedDate}</span>
                    </div>
                  </div>
                  <button
                    onClick={(e) => handleDeleteSession(s.session_id, e)}
                    className="opacity-0 group-hover/item:opacity-100 p-1 rounded-lg hover:bg-red-50 hover:text-red-600 text-slate-400 transition-all cursor-pointer"
                    title="Delete session"
                  >
                    <Trash2 size={14} />
                  </button>
                </div>
              );
            })
          )}
        </div>
      </div>

      {/* Right Pane Editor Canvas */}
      <div className="flex-1 flex flex-col overflow-hidden bg-white print:!block print:!h-auto print:!overflow-visible">
        {showSuccessFlash ? (
          <div className="fixed right-6 top-6 z-50 pointer-events-none">
            <div className="flex items-center gap-2 rounded-2xl border border-emerald-200 bg-emerald-50/95 px-4 py-3 shadow-lg shadow-emerald-950/10 backdrop-blur-sm">
              <Check size={16} className="text-emerald-600" />
              <span className="text-sm font-medium text-emerald-900">PDF saved</span>
            </div>
          </div>
        ) : null}
        {selectedSession ? (
          <>
            {/* Editor Action Header */}
            <div className="h-16 px-6 border-b border-slate-200 flex items-center justify-between shrink-0 bg-slate-50/30 print:hidden print-hide-header">
              <input
                type="text"
                value={titleInput}
                onChange={(e) => {
                  setTitleInput(e.target.value);
                  titleInputRef.current = e.target.value;
                  if (editor) void scheduleSessionSave(editor.getHTML());
                }}
                className="font-bold text-lg text-slate-800 border-none outline-none bg-transparent focus:ring-0 w-2/3 font-sans"
                placeholder="Session Title"
              />

              <div className="flex items-center gap-2">
                <button
                  onClick={handleManualSave}
                  className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg border border-slate-200 hover:bg-slate-50 text-slate-600 text-sm font-semibold cursor-pointer transition-colors font-sans"
                >
                  {saving ? (
                    <>
                      <Check size={14} className="text-green-500" />
                      <span className="text-green-600">Saved</span>
                    </>
                  ) : (
                    <>
                      <Save size={14} />
                      <span>Save</span>
                    </>
                  )}
                </button>

                <button
                  onClick={handleExportPDF}
                  disabled={exporting}
                  className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-lg bg-blue-600 hover:bg-blue-700 disabled:bg-blue-400 text-white text-sm font-bold cursor-pointer transition-colors shadow-xs font-sans"
                >
                  {exporting ? (
                    <>
                      <Loader2 size={14} className="animate-spin" />
                      <span>Compiling...</span>
                    </>
                  ) : (
                    <>
                      <FileDown size={14} />
                      <span>PDF</span>
                    </>
                  )}
                </button>
              </div>
            </div>

            {/* Formatting Toolbar */}
            <div
              onPointerDown={handleToolbarPointerDown}
              className="border-b border-slate-200 bg-slate-50/50 p-2 flex flex-wrap items-center gap-1 shrink-0 select-none print:hidden print-hide-toolbar"
            >
              {/* Undo / Redo */}
              <div className="flex items-center gap-0.5 border-r border-slate-200 pr-1.5 mr-1.5">
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().undo().run()}
                  disabled={!toolbarState?.canUndo}
                  className="p-1.5 rounded-lg hover:bg-slate-200/60 text-slate-600 disabled:opacity-30 cursor-pointer transition-colors"
                  title="Undo"
                >
                  <Undo2 size={16} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().redo().run()}
                  disabled={!toolbarState?.canRedo}
                  className="p-1.5 rounded-lg hover:bg-slate-200/60 text-slate-600 disabled:opacity-30 cursor-pointer transition-colors"
                  title="Redo"
                >
                  <Redo2 size={16} />
                </button>
              </div>

              {/* Headings & Text Styles */}
              <div className="flex items-center gap-1 border-r border-slate-200 pr-1.5 mr-1.5">
                <select
                  onPointerDown={captureSelection}
                  onFocus={captureSelection}
                  value={toolbarState?.heading || "p"}
                  onChange={(e) => {
                    restoreCapturedSelection();
                    const val = e.target.value;
                    if (val === 'p') editor?.chain().focus().setParagraph().run();
                    else if (val === 'h1') editor?.chain().focus().toggleHeading({ level: 1 }).run();
                    else if (val === 'h2') editor?.chain().focus().toggleHeading({ level: 2 }).run();
                    else if (val === 'h3') editor?.chain().focus().toggleHeading({ level: 3 }).run();
                  }}
                  className="text-xs border border-slate-200 bg-white rounded-md px-2 py-1.5 outline-none text-slate-800 font-sans cursor-pointer hover:border-slate-350 transition-colors font-medium"
                >
                  <option value="p">Normal Text</option>
                  <option value="h1">Heading 1</option>
                  <option value="h2">Heading 2</option>
                  <option value="h3">Heading 3</option>
                </select>
              </div>

              {/* Font Size Selector */}
              <div className="flex items-center gap-0.5 border-r border-slate-200 pr-1.5 mr-1.5">
                <select
                  onPointerDown={captureSelection}
                  onFocus={captureSelection}
                  value={toolbarState?.fontSize || '16px'}
                  onChange={(e) => {
                    restoreCapturedSelection();
                    const size = e.target.value;
                    if (size === "mixed") return;
                    if (size === 'default') {
                      editor?.chain().focus().unsetMark('fontSize').run();
                    } else {
                      editor?.chain().focus().setMark('fontSize', { size }).run();
                    }
                  }}
                  className="text-xs border border-slate-200 bg-white rounded-md px-2 py-1.5 outline-none text-slate-800 font-sans cursor-pointer hover:border-slate-350 transition-colors font-medium"
                >
                  <option value="mixed" disabled>Mixed</option>
                  <option value="12px">12px</option>
                  <option value="14px">14px</option>
                  <option value="16px">16px (Default)</option>
                  <option value="18px">18px</option>
                  <option value="20px">20px</option>
                  <option value="24px">24px</option>
                  <option value="30px">30px</option>
                  <option value="36px">36px</option>
                  <option value="48px">48px</option>
                </select>

                {/* Font Size Increase / Decrease Buttons */}
                <button
                  type="button"
                  onClick={() => {
                    const current = toolbarState?.fontSize === "mixed" ? "16px" : toolbarState?.fontSize || '16px';
                    const num = parseInt(current, 10) || 16;
                    const next = Math.min(72, num + 2);
                    editor?.chain().focus().setMark('fontSize', { size: `${next}px` }).run();
                  }}
                  className="p-1.5 rounded-lg hover:bg-slate-200/60 text-slate-700 font-bold text-xs cursor-pointer transition-colors"
                  title="Increase Font Size"
                >
                  A<sup>+</sup>
                </button>
                <button
                  type="button"
                  onClick={() => {
                    const current = toolbarState?.fontSize === "mixed" ? "16px" : toolbarState?.fontSize || '16px';
                    const num = parseInt(current, 10) || 16;
                    const next = Math.max(8, num - 2);
                    editor?.chain().focus().setMark('fontSize', { size: `${next}px` }).run();
                  }}
                  className="p-1.5 rounded-lg hover:bg-slate-200/60 text-slate-700 font-bold text-xs cursor-pointer transition-colors"
                  title="Decrease Font Size"
                >
                  A<sup>-</sup>
                </button>
              </div>

              {/* Bold, Italic, Underline, Strikethrough */}
              <div className="flex items-center gap-0.5 border-r border-slate-200 pr-1.5 mr-1.5">
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleBold().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.bold ? 'bg-blue-100 text-blue-700 font-bold' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Bold"
                >
                  <Bold size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleItalic().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.italic ? 'bg-blue-100 text-blue-700 font-bold' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Italic"
                >
                  <Italic size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleUnderline().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.underline ? 'bg-blue-100 text-blue-700 font-bold' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Underline"
                >
                  <UnderlineIcon size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleStrike().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.strike ? 'bg-blue-100 text-blue-700 font-bold' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Strikethrough"
                >
                  <Strikethrough size={15} />
                </button>
              </div>

              {/* Text Alignments */}
              <div className="flex items-center gap-0.5 border-r border-slate-200 pr-1.5 mr-1.5">
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().setTextAlign('left').run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.textAlign === 'left' ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Align Left"
                >
                  <AlignLeft size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().setTextAlign('center').run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.textAlign === 'center' ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Align Center"
                >
                  <AlignCenter size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().setTextAlign('right').run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.textAlign === 'right' ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Align Right"
                >
                  <AlignRight size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().setTextAlign('justify').run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.textAlign === 'justify' ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Align Justify"
                >
                  <AlignJustify size={15} />
                </button>
              </div>

              {/* Lists */}
              <div className="flex items-center gap-0.5 border-r border-slate-200 pr-1.5 mr-1.5">
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleBulletList().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.bulletList ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Bullet List"
                >
                  <List size={15} />
                </button>
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleOrderedList().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.orderedList ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Numbered List"
                >
                  <ListOrdered size={15} />
                </button>
              </div>

              {/* Blockquote */}
              <div className="flex items-center gap-0.5">
                <button
                  type="button"
                  onClick={() => editor?.chain().focus().toggleBlockquote().run()}
                  className={`p-1.5 rounded-lg cursor-pointer transition-colors ${
                    toolbarState?.blockquote ? 'bg-blue-100 text-blue-700' : 'hover:bg-slate-200/60 text-slate-600'
                  }`}
                  title="Blockquote"
                >
                  <Quote size={15} />
                </button>
              </div>
            </div>

            {/* Hint Dropzone Area */}
            <div 
              onDragOver={(e) => e.preventDefault()}
              onDrop={handleDropVerse}
              className="flex-1 overflow-y-auto p-8 relative group print:!block print:!h-auto print:!overflow-visible print:!max-h-none print:!p-0 print-expand-editor"
            >
              <EditorContent editor={editor} className="session-editor h-full print:!block print:!h-auto print:!overflow-visible" />
            </div>

          </>
        ) : (
          <div className="flex-1 flex flex-col items-center justify-center text-slate-400 font-sans gap-3">
            <Notebook size={40} className="text-slate-300 animate-pulse" />
            <p className="font-semibold text-slate-700 text-lg">No session selected</p>
            <p className="text-sm text-slate-500 max-w-[280px] text-center leading-relaxed font-sans">Select a study session from the sidebar or create a new one to begin taking structured exegesis logs.</p>
          </div>
        )}
      </div>
    </div>
  );
}
