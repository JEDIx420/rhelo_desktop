use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use unicode_normalization::UnicodeNormalization;

use crate::DatabaseState;

const MAX_TRANSCRIPT_CHARACTERS: usize = 20_000;
const MAX_SUGGESTIONS: usize = 50;
const MAX_CANDIDATES_PER_TOKEN: usize = 3;

const BIBLE_BOOK_NAMES: &[&str] = &[
    "Genesis",
    "Exodus",
    "Leviticus",
    "Numbers",
    "Deuteronomy",
    "Joshua",
    "Judges",
    "Ruth",
    "Samuel",
    "Kings",
    "Chronicles",
    "Ezra",
    "Nehemiah",
    "Esther",
    "Job",
    "Psalms",
    "Proverbs",
    "Ecclesiastes",
    "Solomon",
    "Isaiah",
    "Jeremiah",
    "Lamentations",
    "Ezekiel",
    "Daniel",
    "Hosea",
    "Joel",
    "Amos",
    "Obadiah",
    "Jonah",
    "Micah",
    "Nahum",
    "Habakkuk",
    "Zephaniah",
    "Haggai",
    "Zechariah",
    "Malachi",
    "Matthew",
    "Mark",
    "Luke",
    "John",
    "Acts",
    "Romans",
    "Corinthians",
    "Galatians",
    "Ephesians",
    "Philippians",
    "Colossians",
    "Thessalonians",
    "Timothy",
    "Titus",
    "Philemon",
    "Hebrews",
    "James",
    "Peter",
    "Jude",
    "Revelation",
];

const COMMON_WORDS: &[&str] = &[
    "about",
    "after",
    "again",
    "also",
    "another",
    "because",
    "before",
    "being",
    "between",
    "could",
    "every",
    "first",
    "from",
    "good",
    "great",
    "have",
    "into",
    "just",
    "know",
    "like",
    "little",
    "made",
    "make",
    "many",
    "more",
    "most",
    "much",
    "other",
    "people",
    "right",
    "said",
    "same",
    "should",
    "some",
    "something",
    "still",
    "than",
    "that",
    "their",
    "them",
    "then",
    "there",
    "these",
    "they",
    "thing",
    "think",
    "this",
    "those",
    "through",
    "time",
    "under",
    "very",
    "want",
    "were",
    "what",
    "when",
    "where",
    "which",
    "while",
    "will",
    "with",
    "without",
    "would",
    "your",
];

#[derive(Clone, Debug, PartialEq, Eq)]
struct DatabaseIdentity {
    length: u64,
    modified_nanos: u128,
}

#[derive(Clone, Debug)]
struct VocabularyCandidate {
    term: String,
    normalized: String,
    category: &'static str,
    priority: u8,
    occurrences: usize,
}

#[derive(Debug)]
struct VocabularyIndex {
    by_initial: HashMap<char, Vec<VocabularyCandidate>>,
    exact: HashSet<String>,
    size: usize,
}

#[derive(Debug)]
struct CachedVocabulary {
    identity: DatabaseIdentity,
    index: Arc<VocabularyIndex>,
}

#[derive(Clone, Default)]
pub(crate) struct BiblicalVocabularyState {
    cache: Arc<Mutex<Option<CachedVocabulary>>>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BiblicalSuggestionCandidate {
    term: String,
    category: String,
    distance: usize,
    rank: usize,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BiblicalTermSuggestion {
    original: String,
    start: usize,
    end: usize,
    candidates: Vec<BiblicalSuggestionCandidate>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BiblicalSuggestionResponse {
    suggestions: Vec<BiblicalTermSuggestion>,
    supported: bool,
    language: &'static str,
    truncated: bool,
    vocabulary_size: usize,
    initialization_ms: u128,
    matching_ms: u128,
}

#[derive(Debug)]
struct TranscriptToken {
    original: String,
    start: usize,
    end: usize,
    normalized: String,
}

#[tauri::command]
pub(crate) async fn suggest_biblical_terms(
    transcript: String,
    max_suggestions: Option<usize>,
    database: tauri::State<'_, DatabaseState>,
    vocabulary: tauri::State<'_, BiblicalVocabularyState>,
) -> Result<BiblicalSuggestionResponse, String> {
    if transcript.chars().count() > MAX_TRANSCRIPT_CHARACTERS {
        return Err(format!(
            "The transcript is too long to check. The limit is {MAX_TRANSCRIPT_CHARACTERS} characters."
        ));
    }

    let database_path = database.path.clone();
    let cache = vocabulary.cache.clone();
    tauri::async_runtime::spawn_blocking(move || {
        suggest_from_database(
            &database_path,
            &cache,
            &transcript,
            max_suggestions.unwrap_or(20).clamp(1, MAX_SUGGESTIONS),
        )
    })
    .await
    .map_err(|_| "The local Biblical vocabulary check could not be completed.".to_string())?
    .map_err(|_| "The local Biblical vocabulary is unavailable.".to_string())
}

fn suggest_from_database(
    database_path: &Path,
    cache: &Mutex<Option<CachedVocabulary>>,
    transcript: &str,
    max_suggestions: usize,
) -> Result<BiblicalSuggestionResponse, String> {
    let initialization_started = Instant::now();
    let identity = database_identity(database_path)?;
    let (index, initialized) = {
        let mut guard = cache
            .lock()
            .map_err(|_| "The vocabulary cache is unavailable.".to_string())?;
        if let Some(cached) = guard.as_ref().filter(|cached| cached.identity == identity) {
            (cached.index.clone(), false)
        } else {
            let connection = Connection::open_with_flags(
                database_path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(|error| format!("Failed to open vocabulary database: {error}"))?;
            let index = Arc::new(load_vocabulary(&connection)?);
            *guard = Some(CachedVocabulary {
                identity,
                index: index.clone(),
            });
            (index, true)
        }
    };
    let initialization_ms = if initialized {
        initialization_started.elapsed().as_millis()
    } else {
        0
    };

    let matching_started = Instant::now();
    let supported = transcript
        .chars()
        .filter(|character| character.is_alphabetic())
        .all(is_supported_latin_character);
    let suggestions = if supported {
        suggest_with_index(&index, transcript, max_suggestions)
    } else {
        Vec::new()
    };

    Ok(BiblicalSuggestionResponse {
        suggestions,
        supported,
        language: "en-Latn",
        truncated: false,
        vocabulary_size: index.size,
        initialization_ms,
        matching_ms: matching_started.elapsed().as_millis(),
    })
}

fn database_identity(path: &Path) -> Result<DatabaseIdentity, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Failed to inspect vocabulary data: {error}"))?;
    let modified_nanos = metadata
        .modified()
        .unwrap_or(SystemTime::UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(DatabaseIdentity {
        length: metadata.len(),
        modified_nanos,
    })
}

fn load_vocabulary(connection: &Connection) -> Result<VocabularyIndex, String> {
    let mut candidates = HashMap::<String, VocabularyCandidate>::new();
    for name in BIBLE_BOOK_NAMES {
        insert_candidate(&mut candidates, name, "Bible book", 0);
    }
    load_names(
        connection,
        "SELECT name FROM people",
        "Person",
        1,
        &mut candidates,
    )?;
    load_names(
        connection,
        "SELECT name FROM geography_places",
        "Place",
        2,
        &mut candidates,
    )?;
    load_names(
        connection,
        "SELECT name FROM bible_names_dictionary",
        "Biblical term",
        3,
        &mut candidates,
    )?;
    load_names(
        connection,
        "SELECT name FROM dictionary_entries",
        "Biblical term",
        4,
        &mut candidates,
    )?;

    let exact = candidates.keys().cloned().collect::<HashSet<_>>();
    let size = candidates.len();
    let mut by_initial = HashMap::<char, Vec<VocabularyCandidate>>::new();
    for candidate in candidates.into_values() {
        if let Some(initial) = candidate.normalized.chars().next() {
            by_initial.entry(initial).or_default().push(candidate);
        }
    }
    for bucket in by_initial.values_mut() {
        bucket.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then_with(|| left.normalized.cmp(&right.normalized))
        });
    }

    Ok(VocabularyIndex {
        by_initial,
        exact,
        size,
    })
}

fn load_names(
    connection: &Connection,
    sql: &str,
    category: &'static str,
    priority: u8,
    candidates: &mut HashMap<String, VocabularyCandidate>,
) -> Result<(), String> {
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("Failed to prepare vocabulary source: {error}"))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Failed to read vocabulary source: {error}"))?;
    for name in names {
        let name = name.map_err(|error| format!("Failed to decode vocabulary term: {error}"))?;
        insert_candidate(candidates, &name, category, priority);
    }
    Ok(())
}

fn insert_candidate(
    candidates: &mut HashMap<String, VocabularyCandidate>,
    term: &str,
    category: &'static str,
    priority: u8,
) {
    let trimmed = term.trim();
    let normalized = normalize_term(trimmed);
    if !is_usable_candidate(trimmed, &normalized, priority) {
        return;
    }

    candidates
        .entry(normalized.clone())
        .and_modify(|candidate| {
            candidate.occurrences += 1;
            if priority < candidate.priority {
                candidate.term = trimmed.to_string();
                candidate.category = category;
                candidate.priority = priority;
            }
        })
        .or_insert_with(|| VocabularyCandidate {
            term: trimmed.to_string(),
            normalized,
            category,
            priority,
            occurrences: 1,
        });
}

fn is_usable_candidate(term: &str, normalized: &str, priority: u8) -> bool {
    if normalized.len() < 4 || normalized.len() > 32 {
        return false;
    }
    if term
        .chars()
        .any(|character| !(character.is_alphabetic() || matches!(character, '-' | '\'' | '’')))
    {
        return false;
    }
    if priority == 4 {
        if normalized.len() < 6 || COMMON_WORDS.contains(&normalized) {
            return false;
        }
        if !term.chars().next().is_some_and(char::is_uppercase) {
            return false;
        }
    }
    true
}

fn suggest_with_index(
    index: &VocabularyIndex,
    transcript: &str,
    max_suggestions: usize,
) -> Vec<BiblicalTermSuggestion> {
    let common = COMMON_WORDS.iter().copied().collect::<HashSet<_>>();
    tokenize(transcript)
        .into_iter()
        .filter(|token| token.normalized.len() >= 4)
        .filter(|token| !common.contains(token.normalized.as_str()))
        .filter(|token| !index.exact.contains(&token.normalized))
        .filter_map(|token| {
            let initial = token.normalized.chars().next()?;
            let bucket = index.by_initial.get(&initial)?;
            let mut ranked = bucket
                .iter()
                .filter_map(|candidate| rank_candidate(&token.normalized, candidate))
                .collect::<Vec<_>>();
            ranked.sort_by(|left, right| {
                left.rank
                    .cmp(&right.rank)
                    .then_with(|| left.distance.cmp(&right.distance))
                    .then_with(|| left.term.cmp(&right.term))
            });
            ranked.dedup_by(|left, right| left.term.eq_ignore_ascii_case(&right.term));
            ranked.truncate(MAX_CANDIDATES_PER_TOKEN);
            if ranked.is_empty() {
                return None;
            }
            Some(BiblicalTermSuggestion {
                original: token.original,
                start: token.start,
                end: token.end,
                candidates: ranked,
            })
        })
        .take(max_suggestions)
        .collect()
}

fn rank_candidate(
    token: &str,
    candidate: &VocabularyCandidate,
) -> Option<BiblicalSuggestionCandidate> {
    let length_difference = token.len().abs_diff(candidate.normalized.len());
    let maximum_distance = match token.len() {
        0..=4 => 1,
        5..=7 => 2,
        8..=11 => 3,
        _ => 4,
    };
    if length_difference > maximum_distance {
        return None;
    }

    let distance = damerau_levenshtein(token, &candidate.normalized);
    if distance == 0 || distance > maximum_distance {
        return None;
    }
    let longest = token.len().max(candidate.normalized.len()) as f64;
    let similarity = 1.0 - (distance as f64 / longest);
    let threshold = match candidate.priority {
        0 => 0.72,
        1..=3 => 0.78,
        _ => 0.88,
    };
    let phonetic_match = soundex(token) == soundex(&candidate.normalized);
    if similarity < threshold || (!phonetic_match && distance == maximum_distance) {
        return None;
    }

    let rank = distance * 100 + candidate.priority as usize * 12 + length_difference * 4
        - candidate.occurrences.min(10);
    Some(BiblicalSuggestionCandidate {
        term: candidate.term.clone(),
        category: candidate.category.to_string(),
        distance,
        rank,
    })
}

fn tokenize(transcript: &str) -> Vec<TranscriptToken> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut start_utf16 = 0;
    let mut utf16_offset = 0;

    for character in transcript.chars() {
        let is_word_character = character.is_alphabetic()
            || (!current.is_empty() && matches!(character, '-' | '\'' | '’'));
        if is_word_character {
            if current.is_empty() {
                start_utf16 = utf16_offset;
            }
            current.push(character);
        } else if !current.is_empty() {
            push_token(&mut tokens, &mut current, start_utf16);
        }
        utf16_offset += character.len_utf16();
    }
    if !current.is_empty() {
        push_token(&mut tokens, &mut current, start_utf16);
    }
    tokens
}

fn push_token(tokens: &mut Vec<TranscriptToken>, current: &mut String, start: usize) {
    while current.ends_with(['-', '\'', '’']) {
        current.pop();
    }
    if !current.is_empty() {
        tokens.push(TranscriptToken {
            original: current.clone(),
            start,
            end: start + current.encode_utf16().count(),
            normalized: normalize_term(current),
        });
    }
    current.clear();
}

fn normalize_term(term: &str) -> String {
    term.nfkd()
        .filter(|character| character.is_ascii_alphabetic())
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_supported_latin_character(character: char) -> bool {
    character
        .to_string()
        .nfkd()
        .any(|part| part.is_ascii_alphabetic())
}

fn soundex(value: &str) -> String {
    let mut characters = value.chars().filter(char::is_ascii_alphabetic);
    let Some(first) = characters.next() else {
        return String::new();
    };
    let mut result = String::from(first.to_ascii_uppercase());
    let mut previous = soundex_code(first);
    for character in characters {
        let code = soundex_code(character);
        if code != '0' && code != previous {
            result.push(code);
            if result.len() == 4 {
                break;
            }
        }
        previous = code;
    }
    while result.len() < 4 {
        result.push('0');
    }
    result
}

fn soundex_code(character: char) -> char {
    match character.to_ascii_lowercase() {
        'b' | 'f' | 'p' | 'v' => '1',
        'c' | 'g' | 'j' | 'k' | 'q' | 's' | 'x' | 'z' => '2',
        'd' | 't' => '3',
        'l' => '4',
        'm' | 'n' => '5',
        'r' => '6',
        _ => '0',
    }
}

fn damerau_levenshtein(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut matrix = vec![vec![0; right.len() + 1]; left.len() + 1];
    for (index, row) in matrix.iter_mut().enumerate() {
        row[0] = index;
    }
    for (index, distance) in matrix[0].iter_mut().enumerate() {
        *distance = index;
    }

    for left_index in 1..=left.len() {
        for right_index in 1..=right.len() {
            let substitution_cost = usize::from(left[left_index - 1] != right[right_index - 1]);
            matrix[left_index][right_index] = (matrix[left_index - 1][right_index] + 1)
                .min(matrix[left_index][right_index - 1] + 1)
                .min(matrix[left_index - 1][right_index - 1] + substitution_cost);
            if left_index > 1
                && right_index > 1
                && left[left_index - 1] == right[right_index - 2]
                && left[left_index - 2] == right[right_index - 1]
            {
                matrix[left_index][right_index] = matrix[left_index][right_index]
                    .min(matrix[left_index - 2][right_index - 2] + 1);
            }
        }
    }
    matrix[left.len()][right.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn fixture_index() -> VocabularyIndex {
        let mut candidates = HashMap::new();
        for (term, category, priority) in [
            ("Nebuchadnezzar", "Person", 1),
            ("Melchizedek", "Person", 1),
            ("Bartholomew", "Person", 1),
            ("Capernaum", "Place", 2),
            ("Deuteronomy", "Bible book", 0),
            ("Thessalonians", "Bible book", 0),
        ] {
            insert_candidate(&mut candidates, term, category, priority);
        }
        let exact = candidates.keys().cloned().collect();
        let size = candidates.len();
        let mut by_initial = HashMap::<char, Vec<VocabularyCandidate>>::new();
        for candidate in candidates.into_values() {
            by_initial
                .entry(candidate.normalized.chars().next().unwrap())
                .or_default()
                .push(candidate);
        }
        VocabularyIndex {
            by_initial,
            exact,
            size,
        }
    }

    #[test]
    fn exact_biblical_terms_do_not_create_suggestions() {
        assert!(suggest_with_index(&fixture_index(), "Nebuchadnezzar", 20).is_empty());
    }

    #[test]
    fn clear_people_places_and_book_misspellings_are_ranked() {
        let suggestions = suggest_with_index(
            &fixture_index(),
            "Nebuchadnezar met Bartholmew near Capernaun before reading Deuteronmy and Thesalonians.",
            20,
        );
        let proposed = suggestions
            .iter()
            .map(|suggestion| {
                (
                    suggestion.original.as_str(),
                    suggestion.candidates[0].term.as_str(),
                    suggestion.candidates[0].category.as_str(),
                )
            })
            .collect::<Vec<_>>();
        assert!(proposed.contains(&("Nebuchadnezar", "Nebuchadnezzar", "Person")));
        assert!(proposed.contains(&("Bartholmew", "Bartholomew", "Person")));
        assert!(proposed.contains(&("Capernaun", "Capernaum", "Place")));
        assert!(proposed.contains(&("Deuteronmy", "Deuteronomy", "Bible book")));
        assert!(proposed.contains(&("Thesalonians", "Thessalonians", "Bible book")));
    }

    #[test]
    fn ordinary_text_and_weak_matches_are_not_flooded() {
        let suggestions = suggest_with_index(
            &fixture_index(),
            "This is an ordinary meeting about product design and software.",
            20,
        );
        assert!(suggestions.is_empty());
    }

    #[test]
    fn punctuation_offsets_and_candidate_limits_are_stable() {
        let suggestions = suggest_with_index(&fixture_index(), "\"Capernaun,\"", 1);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].original, "Capernaun");
        assert_eq!((suggestions[0].start, suggestions[0].end), (1, 10));
        assert!(suggestions[0].candidates.len() <= MAX_CANDIDATES_PER_TOKEN);
    }

    #[test]
    fn unsupported_scripts_are_detected_without_rewriting() {
        assert!(!"മലയാളം"
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(is_supported_latin_character));
        assert!("Capernaum"
            .chars()
            .filter(|c| c.is_alphabetic())
            .all(is_supported_latin_character));
    }

    #[test]
    fn bundled_vocabulary_initialization_and_matching_are_bounded() {
        let database_path =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../rhelo.db");
        if !database_path.exists() {
            return;
        }
        let cache = Mutex::new(None);
        let short_started = Instant::now();
        let short = suggest_from_database(
            &database_path,
            &cache,
            "Nebuchadnezar visited Capernaun and read Deuteronmy.",
            20,
        )
        .unwrap();
        let short_elapsed = short_started.elapsed();
        let long_transcript = "Nebuchadnezar visited Capernaun. ".repeat(250);
        let long_started = Instant::now();
        let long = suggest_from_database(&database_path, &cache, &long_transcript, 50).unwrap();
        let long_elapsed = long_started.elapsed();
        let estimated_heap_bytes = {
            let guard = cache.lock().unwrap();
            let index = &guard.as_ref().unwrap().index;
            let candidates = index
                .by_initial
                .values()
                .flatten()
                .map(|candidate| {
                    std::mem::size_of::<VocabularyCandidate>()
                        + candidate.term.capacity()
                        + candidate.normalized.capacity()
                })
                .sum::<usize>();
            let exact_terms = index
                .exact
                .iter()
                .map(|term| std::mem::size_of::<String>() + term.capacity())
                .sum::<usize>();
            candidates + exact_terms
        };

        eprintln!(
            "vocabulary={} init={}ms short={}ms long={}ms estimated_heap={}KiB",
            short.vocabulary_size,
            short.initialization_ms,
            short_elapsed.as_millis(),
            long_elapsed.as_millis(),
            estimated_heap_bytes / 1024
        );
        assert!(!short.suggestions.is_empty());
        assert_eq!(long.suggestions.len(), 50);
        assert!(short_elapsed < Duration::from_secs(5));
        assert!(long_elapsed < Duration::from_secs(5));
    }
}
