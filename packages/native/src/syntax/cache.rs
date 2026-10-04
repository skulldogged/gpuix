//! Bounded cache for neutral syntax documents.
//!
//! Ported from Comet (https://github.com/zeronsh/comet), MIT.
//! Original: `crates/ui/src/syntax_cache.rs`.
//!
//! Colours and gpui runs deliberately stay OUTSIDE this cache. A theme change
//! then recolours existing spans without reparsing, and one cached document
//! serves both appearances.
//!
//! GPUIX is immediate-mode: `<code>` re-renders on every frame. Without this
//! cache a 200-line snippet is reparsed 60 times a second.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use super::{highlight, HighlightRequest, HighlightSpan, HighlightedDocument, LanguageId};

/// Bump when the scope map or Syntect syntax dump changes, so stale entries
/// from a previous build of the same process cannot be served.
pub const QUERY_GENERATION: u32 = 2;
// A long conversation holds more code blocks than 96, and scrolling back to
// one that fell out highlighted it again.
const MAX_DOCUMENTS: usize = 1024;
const MAX_RETAINED_BYTES: usize = 24 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DocumentKey {
    language: Option<LanguageId>,
    /// For a provider's highlight, the fence tag and path it was asked with.
    request: u64,
    content_hash: u64,
    content_len: usize,
    query_generation: u32,
}

impl DocumentKey {
    fn new(language: LanguageId, source: &str) -> Self {
        Self {
            language: Some(language),
            request: 0,
            // A 64-bit SipHash from the standard library. Comet uses SHA-256;
            // a collision here paints the wrong colours for one snippet, never
            // corrupts memory, and the length is mixed in as a second check.
            content_hash: hash64(source),
            content_len: source.len(),
            query_generation: QUERY_GENERATION,
        }
    }

    fn requested(fence_tag: Option<&str>, path: Option<&str>, source: &str) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (fence_tag, path).hash(&mut hasher);
        Self {
            language: None,
            request: hasher.finish(),
            content_hash: hash64(source),
            content_len: source.len(),
            query_generation: QUERY_GENERATION,
        }
    }
}

fn hash64(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

struct CachedDocument {
    retained_bytes: usize,
    document: Arc<HighlightedDocument>,
    last_used: u64,
}

#[derive(Default)]
pub struct SyntaxCache {
    documents: HashMap<DocumentKey, CachedDocument>,
    /// Moves on every lookup; the entry used longest ago is evicted first.
    clock: u64,
    retained_bytes: usize,
    hits: u64,
    misses: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub documents: usize,
    pub retained_bytes: usize,
}

impl SyntaxCache {
    fn get(&mut self, key: &DocumentKey) -> Option<Arc<HighlightedDocument>> {
        self.clock += 1;
        let Some(entry) = self.documents.get_mut(key) else {
            self.misses += 1;
            return None;
        };
        self.hits += 1;
        entry.last_used = self.clock;
        Some(entry.document.clone())
    }

    fn insert(&mut self, key: DocumentKey, document: Arc<HighlightedDocument>) {
        if let Some(previous) = self.documents.remove(&key) {
            self.retained_bytes = self.retained_bytes.saturating_sub(previous.retained_bytes);
        }
        let retained_bytes = estimated_bytes(&document);
        if retained_bytes > MAX_RETAINED_BYTES {
            return;
        }
        self.clock += 1;
        self.retained_bytes = self.retained_bytes.saturating_add(retained_bytes);
        self.documents.insert(
            key,
            CachedDocument {
                retained_bytes,
                document,
                last_used: self.clock,
            },
        );
        // Only an insert past a limit scans for the oldest; lookups, which
        // happen for every code block on every frame, stay constant time.
        while self.documents.len() > MAX_DOCUMENTS || self.retained_bytes > MAX_RETAINED_BYTES {
            let Some(oldest) = self
                .documents
                .iter()
                .min_by_key(|(_, entry)| entry.last_used)
                .map(|(key, _)| *key)
            else {
                break;
            };
            if let Some(removed) = self.documents.remove(&oldest) {
                self.retained_bytes = self.retained_bytes.saturating_sub(removed.retained_bytes);
            }
        }
    }

    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            documents: self.documents.len(),
            retained_bytes: self.retained_bytes,
        }
    }
}

fn estimated_bytes(document: &HighlightedDocument) -> usize {
    let spans: usize = document.lines.iter().map(|line| line.len()).sum();
    std::mem::size_of::<HighlightedDocument>()
        + document.lines.len() * std::mem::size_of::<Vec<super::HighlightSpan>>()
        + spans * std::mem::size_of::<super::HighlightSpan>()
}

fn global() -> &'static Mutex<SyntaxCache> {
    static CACHE: OnceLock<Mutex<SyntaxCache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(SyntaxCache::default()))
}

/// Highlight through the process-wide cache.
///
/// Returns `None` when the language is unknown or the source is too large,
/// which callers render as plain text. Negative results are NOT cached: they
/// are cheap to recompute and caching them would keep unparseable megabytes
/// keyed forever.
pub fn highlight_cached(
    source: &str,
    path: Option<&str>,
    fence_tag: Option<&str>,
) -> Option<Arc<HighlightedDocument>> {
    let language = super::detect_language(path, fence_tag, source.lines().next())?;
    let key = DocumentKey::new(language, source);
    if let Some(cached) = global().lock().ok()?.get(&key) {
        return Some(cached);
    }
    let document = highlight(HighlightRequest {
        source,
        path,
        fence_tag,
    })
    .ok()?;
    let document = Arc::new(document);
    if let Ok(mut cache) = global().lock() {
        cache.insert(key, document.clone());
    }
    Some(document)
}

/// A cached highlight, or where one is.
pub enum Lookup {
    Ready(Arc<HighlightedDocument>),
    /// Asked of the provider; the window redraws when it answers.
    Pending,
    /// An unknown language, or a source too large to highlight.
    Unsupported,
}

/// A code block for a provider to highlight. It answers through [`fulfill`]
/// with the same `id`.
pub struct Request {
    pub id: u32,
    pub code: String,
    pub language: Option<String>,
    pub path: Option<String>,
}

pub type Provider = Arc<dyn Fn(Request) + Send + Sync>;

#[derive(Default)]
struct Requests {
    provider: Option<Provider>,
    next_id: u32,
    /// Each outstanding request's key, and the source its spans index into.
    pending: HashMap<u32, (DocumentKey, String)>,
    asked: HashSet<DocumentKey>,
}

/// A provider that never answers would otherwise hold every block it was
/// asked for; past this many, the outstanding ones are dropped and asked
/// again when they next paint.
const MAX_PENDING: usize = 256;

fn requests() -> &'static Mutex<Requests> {
    static REQUESTS: OnceLock<Mutex<Requests>> = OnceLock::new();
    REQUESTS.get_or_init(Mutex::default)
}

/// Hand highlighting to `provider`, which works elsewhere, such as on a
/// JavaScript thread, and answers each request with [`fulfill`]. With none,
/// [`lookup`] highlights with Syntect in place.
pub fn set_provider(provider: Option<Provider>) {
    if let Ok(mut requests) = requests().lock() {
        requests.provider = provider;
        requests.pending.clear();
        requests.asked.clear();
    }
}

/// The highlight for a code block. With a provider set, a miss asks it and
/// returns [`Lookup::Pending`]; the block paints plain, or with an earlier
/// highlight, until the answer lands. Every run of a line uses one font
/// whatever its colour, so the answer recolours the text without moving it.
pub fn lookup(source: &str, path: Option<&str>, fence_tag: Option<&str>) -> Lookup {
    let provider = requests()
        .lock()
        .ok()
        .and_then(|requests| requests.provider.clone());
    let Some(provider) = provider else {
        return match highlight_cached(source, path, fence_tag) {
            Some(document) => Lookup::Ready(document),
            None => Lookup::Unsupported,
        };
    };
    let key = DocumentKey::requested(fence_tag, path, source);
    if let Some(document) = global().lock().ok().and_then(|mut cache| cache.get(&key)) {
        return Lookup::Ready(document);
    }
    let Ok(mut requests) = requests().lock() else {
        return Lookup::Unsupported;
    };
    if !requests.asked.insert(key) {
        return Lookup::Pending;
    }
    if requests.pending.len() >= MAX_PENDING {
        requests.pending.clear();
        requests.asked.clear();
        requests.asked.insert(key);
    }
    let id = requests.next_id;
    requests.next_id = id.wrapping_add(1);
    requests.pending.insert(id, (key, source.to_string()));
    drop(requests);
    provider(Request {
        id,
        code: source.to_string(),
        language: fence_tag.map(str::to_string),
        path: path.map(str::to_string),
    });
    Lookup::Pending
}

/// Store a provider's answer to request `id`: spans as byte ranges into the
/// code it was sent, or `None` when it can't highlight that code, which then
/// paints plain and isn't asked for again.
pub fn fulfill(id: u32, spans: Option<Vec<HighlightSpan>>) {
    let Some((key, source)) = requests().lock().ok().and_then(|mut requests| {
        let (key, source) = requests.pending.remove(&id)?;
        requests.asked.remove(&key);
        Some((key, source))
    }) else {
        return;
    };
    let document = spans
        .and_then(|spans| HighlightedDocument::from_absolute_spans(None, &source, spans).ok())
        .or_else(|| HighlightedDocument::from_absolute_spans(None, &source, []).ok());
    if let (Some(document), Ok(mut cache)) = (document, global().lock()) {
        cache.insert(key, Arc::new(document));
    }
}

pub fn stats() -> CacheStats {
    global()
        .lock()
        .map(|cache| cache.stats())
        .unwrap_or(CacheStats {
            hits: 0,
            misses: 0,
            documents: 0,
            retained_bytes: 0,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_lookup_hits_the_cache() {
        let before = stats().hits;
        let source = "fn cache_probe() -> u32 { 7 }";
        let a = highlight_cached(source, Some("probe.rs"), None).unwrap();
        let b = highlight_cached(source, Some("probe.rs"), None).unwrap();
        assert!(Arc::ptr_eq(&a, &b), "the same Arc must be served twice");
        assert!(stats().hits > before);
    }

    #[test]
    fn different_sources_do_not_share_an_entry() {
        let a = highlight_cached("let a = 1;", Some("a.rs"), None).unwrap();
        let b = highlight_cached("let bb = 2;", Some("a.rs"), None).unwrap();
        assert!(!Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn unknown_language_returns_none() {
        assert!(highlight_cached("x", Some("notes.txt"), None).is_none());
    }

    #[test]
    fn eviction_keeps_the_cache_bounded() {
        let mut cache = SyntaxCache::default();
        for i in 0..(MAX_DOCUMENTS + 20) {
            let source = format!("let x{i} = {i};");
            let document = highlight(HighlightRequest {
                source: &source,
                path: Some("a.rs"),
                fence_tag: None,
            })
            .unwrap();
            cache.insert(
                DocumentKey::new(LanguageId::Rust, &source),
                Arc::new(document),
            );
        }
        assert!(cache.stats().documents <= MAX_DOCUMENTS);
    }
}
