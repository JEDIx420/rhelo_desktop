import { getStoredEnglishTranslation } from "@/lib/englishTranslations";

const invokeDesktop = async <T>(command: string, args: Record<string, unknown> = {}) => {
  try {
    const { invoke } = await import("@tauri-apps/api/core");
    return await invoke<T>(command, args);
  } catch (error) {
    console.error(`[Rhelo IPC] ${command} failed`, error);
    throw error;
  }
};

export interface MorphologyWord {
  word: string;
  lemma?: string;
  pos?: string;
  parse?: string;
}

export interface ChapterVerse {
  id: string;
  book: string;
  chapter: number;
  verse: number;
  text_en: string;
  text_original: string;
  text_hi: string;
  text_te: string;
  text_ml: string;
  text_ta: string;
  cross_references_count: number;
  places_count: number;
  commentaries: string[];
  morphology: MorphologyWord[];
}

export interface ChapterResponse {
  verses: ChapterVerse[];
  translation_code: string;
}

export async function fetchChapter(book: string, chapter: number): Promise<ChapterResponse> {
  return invokeDesktop<ChapterResponse>("fetch_chapter", {
    book,
    chapter,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface EmbeddedPassageResponse {
  translation_code: string;
  book: string;
  chapter: number;
  verses: Array<{ verse: number; text: string }>;
}

export async function fetchEmbeddedTranslationPassage(
  translationCode: string,
  book: string,
  chapter: number,
): Promise<EmbeddedPassageResponse> {
  return invokeDesktop<EmbeddedPassageResponse>("fetch_translation_passage", {
    translationCode,
    book,
    chapter,
  });
}

export interface ScriptureSearchResult {
  id: string;
  book: string;
  chapter: number;
  verse: number;
  text_en: string;
  rank: number;
}

export interface ScriptureSearchResponse {
  results: ScriptureSearchResult[];
  total: number;
  page: number;
  limit: number;
  matching_books: string[];
  matching_testaments: Array<"OT" | "NT">;
  translation_code: string;
}

export async function searchScriptures(
  query: string,
  filters?: { book?: string; testament?: string; sort?: string; page?: number; limit?: number }
): Promise<ScriptureSearchResponse> {
  return invokeDesktop<ScriptureSearchResponse>("search_scripture", {
    query,
    book: filters?.book,
    testament: filters?.testament,
    sort: filters?.sort,
    page: filters?.page,
    limit: filters?.limit,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface VerseDetailsResponse {
  verse: ChapterVerse;
  commentaries: Array<{ commentary_id: string; text: string }>;
  places: Array<{ name: string; latitude: number | null; longitude: number | null; type: string }>;
  events: Array<{ title: string; year: number; location: string; description: string }>;
  cross_references: Array<{ to_verse: string; votes: number; text_en: string }>;
  translation_code: string;
}

export async function fetchVerseDetails(verseId: string): Promise<VerseDetailsResponse> {
  return invokeDesktop<VerseDetailsResponse>("fetch_verse_details", {
    verseId,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface LexiconEntry {
  strongs_id: string;
  lemma: string;
  definition: string;
}

export interface DictionaryEntry {
  id: number;
  slug: string;
  name: string;
  source: string;
  definition_text: string;
}

export interface LexiconOccurrence {
  id: string;
  book: string;
  chapter: number;
  verse: number;
  text_en: string;
  text_original: string;
}

interface NativeLexiconResponse {
  results: LexiconEntry[];
  occurrences: LexiconOccurrence[];
  lexicon: LexiconEntry[];
  dictionary: DictionaryEntry[];
}

export async function searchLexicon(query: string): Promise<Pick<NativeLexiconResponse, "lexicon" | "dictionary">> {
  return invokeDesktop<NativeLexiconResponse>("lookup_lexicon", {
    wordId: query,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface TopicEntry {
  id: number;
  subject: string;
  entry: string;
}

export interface BibleNameEntry {
  name: string;
  meaning: string;
}

export interface TopicsResponse {
  topics: TopicEntry[];
  names: BibleNameEntry[];
}

export async function searchTopics(query: string): Promise<TopicsResponse> {
  return invokeDesktop<TopicsResponse>("fetch_research_meta", {
    category: "topics",
    value: query,
  });
}

export type DictionaryStudyKind = "strongs" | "dictionary" | "topic" | "name";

export interface DictionaryStudyResponse {
  kind: DictionaryStudyKind;
  id: string;
  title: string;
  subtitle: string | null;
  definition: string;
  related_verses: LexiconOccurrence[];
}

export async function fetchDictionaryStudy(
  kind: DictionaryStudyKind,
  entryId: string
): Promise<DictionaryStudyResponse> {
  return invokeDesktop<DictionaryStudyResponse>("fetch_dictionary_study", {
    kind,
    entryId,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface BiographyProfile {
  id: string;
  name: string;
  sex: string;
  tribe: string | null;
  unique_attribute: string | null;
  notes: string | null;
  children_count: number;
  spouse_count: number;
}

export interface BiographyRelationship {
  relationship_type: string;
  relation_name: string;
  relation_id: string;
  relation_sex: string;
  verse_id: string | null;
  children_count: number;
  spouse_count: number;
}

export interface BiographyResponse {
  profile: BiographyProfile;
  relationships: BiographyRelationship[];
  name_meaning: string | null;
  error?: string;
}

export async function fetchBiography(personId: string): Promise<BiographyResponse> {
  return invokeDesktop<BiographyResponse>("fetch_research_meta", {
    category: "biography",
    value: personId,
  });
}

export interface ChapterMapPlace {
  name: string;
  latitude: number | null;
  longitude: number | null;
  type: string;
  verse_id: string;
  text_en: string;
  text_original: string;
  meaning: string | null;
  commentary: string | null;
  dict_definition: string | null;
}

export async function fetchChapterMap(book: string, chapter: number): Promise<{ places: ChapterMapPlace[] }> {
  return invokeDesktop<{ places: ChapterMapPlace[] }>("fetch_chapter_map", {
    book,
    chapter,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface TimelineEvent {
  event_id: string;
  title: string;
  year: number;
  location: string;
  description: string;
  verses: string[];
}

export interface TimelineResponse {
  events: TimelineEvent[];
}

export async function fetchTimeline(): Promise<TimelineResponse> {
  return invokeDesktop<TimelineResponse>("fetch_research_meta", {
    category: "timeline",
  });
}

export async function lookupLexicon(word: string): Promise<{ results: LexiconEntry[] }> {
  return invokeDesktop<NativeLexiconResponse>("lookup_lexicon", {
    wordId: word,
    translationCode: getStoredEnglishTranslation(),
  });
}

export async function fetchOccurrences(lemma: string): Promise<{ occurrences: LexiconOccurrence[] }> {
  return invokeDesktop<NativeLexiconResponse>("lookup_lexicon", {
    wordId: lemma,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface StatsResponse {
  status: "connected";
  stats: {
    verses: number;
    lexicon: number;
    dictionaries: number;
    places: number;
    events: number;
    people: number;
  };
}

export async function fetchStats(): Promise<StatsResponse> {
  return invokeDesktop<StatsResponse>("fetch_stats");
}

export interface TtsVoiceDescriptor {
  name: string;
  language: string;
}

export interface TtsVoiceSelection {
  requested_language: string;
  normalized_locale: string;
  selected_voice: TtsVoiceDescriptor | null;
  available: boolean;
  reason: string | null;
}

export interface TtsDiagnosticsResponse {
  os: string;
  native_tts_available: boolean;
  initialization_error: string | null;
  current_schema_version: number;
  detected_voices: TtsVoiceDescriptor[];
  english: TtsVoiceSelection;
  greek: TtsVoiceSelection;
  hebrew: TtsVoiceSelection;
}

export async function fetchTtsDiagnostics(): Promise<TtsDiagnosticsResponse> {
  return invokeDesktop<TtsDiagnosticsResponse>("fetch_tts_diagnostics");
}

export interface GeographyRoute {
  route_id: string;
  title: string;
  description: string;
}

export async function fetchGeographyRoutes(): Promise<{ routes: GeographyRoute[] }> {
  return invokeDesktop<{ routes: GeographyRoute[] }>("fetch_geography_routes");
}

export interface RoutePoint {
  sequence_order: number;
  latitude: number;
  longitude: number;
  place_name: string;
  associated_verse_id: string;
  text_en: string;
  text_original: string;
}

export async function fetchRoutePoints(routeId: string): Promise<{ points: RoutePoint[] }> {
  return invokeDesktop<{ points: RoutePoint[] }>("fetch_route_points", {
    routeId,
    translationCode: getStoredEnglishTranslation(),
  });
}

export interface SessionRecord {
  session_id: string;
  title: string;
  content: string;
  updated_at: string;
}

export async function fetchSessions(): Promise<{ sessions: SessionRecord[] }> {
  return invokeDesktop<{ sessions: SessionRecord[] }>("fetch_sessions");
}

export async function createSession(title: string, content: string) {
  return invokeDesktop<{ status: string; session_id: string; title: string; content: string }>(
    "create_session",
    { title, content },
  );
}

export async function updateSession(sessionId: string, title: string, content: string) {
  return invokeDesktop<{ status: string; session: SessionRecord }>("update_session", {
    sessionId,
    title,
    content,
  });
}

export async function deleteSession(sessionId: string) {
  return invokeDesktop<{ status: string; session_id: string }>("delete_session", { sessionId });
}

export type BiblicalSuggestionCategory =
  | "Bible book"
  | "Person"
  | "Place"
  | "Biblical term";

export interface BiblicalSuggestionCandidate {
  term: string;
  category: BiblicalSuggestionCategory;
  distance: number;
  rank: number;
}

export interface BiblicalTermSuggestion {
  original: string;
  start: number;
  end: number;
  candidates: BiblicalSuggestionCandidate[];
}

export interface BiblicalSuggestionResponse {
  suggestions: BiblicalTermSuggestion[];
  supported: boolean;
  language: "en-Latn";
  truncated: boolean;
  vocabularySize: number;
  initializationMs: number;
  matchingMs: number;
}

export async function suggestBiblicalTerms(
  transcript: string,
  maxSuggestions = 20,
): Promise<BiblicalSuggestionResponse> {
  return invokeDesktop<BiblicalSuggestionResponse>("suggest_biblical_terms", {
    transcript,
    maxSuggestions,
  });
}

export async function searchSessions(query: string): Promise<{ sessions: SessionRecord[] }> {
  return invokeDesktop<{ sessions: SessionRecord[] }>("search_sessions", { query });
}

export interface SessionPDFExportResult {
  saved: boolean;
  path: string | null;
}

export async function exportAndSaveSessionPDF(
  title: string,
  content: string,
): Promise<SessionPDFExportResult> {
  return invokeDesktop<SessionPDFExportResult>("export_and_save_session_pdf", { title, content });
}
