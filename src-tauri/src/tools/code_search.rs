// ── surface: backend ──────────────────────────────────
// Hybrid code search (spec §9): BM25 + bge-small vector arms fused with RRF.
// Exports: search · Related: tools/mod.rs (project_search), archive/DESIGN_OLD.md §9

//! `code_search` — relevance-ranked search over the project's code + docs (spec
//! §9, tier 2). Two retrieval arms, fused with Reciprocal Rank Fusion:
//!
//! - **BM25 (lexical)** — pure Rust, always on. Ranks whole chunks by keyword
//!   relevance, with identifier-aware tokenization so "update scratchpad" matches
//!   `update_scratchpad` / `updateScratchpad`.
//! - **vector (semantic)** — bge-small-en-v1.5 via `fastembed`/ONNX Runtime,
//!   loaded lazily and cached across calls. Bridges the gap where the query's
//!   words aren't the code's words ("where do we decide the scratchpad shows?").
//!
//! Chunks are semantic where Tree-Sitter can see: supported source files are cut
//! at functions, methods, type definitions, and impl/class blocks (via the
//! `tools/symbols` engine), doc comments travel with the item they describe, and
//! only an oversized single item is subdivided along lines. Everything else —
//! plain text, markdown — keeps the blind line-window fallback. Test/mock/
//! fixture content is chunked for BM25 but never embedded (pure CPU bloat for
//! the semantic arm), and a per-file chunk cap keeps generated giants from
//! exploding the embed batches.
//!
//! The vector arm degrades gracefully: if the embedder can't load (no model yet /
//! download failed), search still returns BM25-only results with an honest note.
//! The model downloads once on first use (see Cargo.toml note), then runs fully
//! offline. The vector arm uses an ephemeral two-stage pipeline: BM25 first
//! screens candidate chunks (with length normalization penalizing verbose noise),
//! and bge-small dynamically re-ranks the top candidate pool on demand. Embeddings
//! are cached in a content-addressable LRU keyed by chunk text hash and persisted
//! to the app-data dir, eliminating whole-repo cold build stalls.
//!
//! `search` is a pure function of `(arguments, root, cancel)` — no Tauri state;
//! the cancel token is passed in (not read from anywhere global), so the tests
//! drive it directly. It runs on a blocking thread (see `Tools::code_search`'s
//! `spawn_blocking`), so the synchronous ONNX inference never sits on the
//! async/UI thread — and checks the token at each stage boundary (entry, corpus
//! walk, embed batches) so a Stop halts that thread's CPU burn too.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fastembed::TextEmbedding;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{read_text_capped, Tools};

/// Files walked before we stop (mirrors project_search's cap — a runaway tree
/// must not stall a turn).
const FILE_CAP: usize = 2000;

/// Token budget per chunk — the retrieval "document". Chunks are sized so each
/// fits comfortably under bge-small's 512-token input limit (headroom for the
/// [CLS]/[SEP] special tokens), so the vector arm never silently truncates a
/// chunk's tail. Measured (spec §9): 100 lines of code ≈ 800 bge tokens, which is
/// why the old line-based chunking truncated ~85% of chunks and dropped ~81% of
/// all tokens. This is the ranking-granularity knob: smaller = more precise but
/// more chunks to embed; larger = more context per chunk. With AST-aware
/// chunking it becomes a *merge* budget — whole symbols are packed up to it, and
/// only a single oversized symbol is subdivided at this granularity.
const CHUNK_TOKEN_BUDGET: usize = 450;

/// Chunks embedded per inference call. The cold-index build embeds every changed
/// chunk in one mega-batch today; slicing it gives cancellation a checkpoint
/// (Stop lands within one batch instead of after the whole build) and bounds the
/// padded forward-pass tensor — a batch of thousands of 512-token sequences is
/// both slow to pad and slow to run.
const EMBED_BATCH: usize = 64;

/// Sanity cap on chunks per single file, enforced by merging adjacent chunks
/// past the cap (coverage is never dropped — merged chunks span their combined
/// lines and the vector arm just embeds the leading tokens). This is the
/// backstop for generated/vendored giants exploding the batch count; at 128
/// even a 7k-line hand-written file (~125 chunks at the budget) keeps full
/// chunk fidelity.
const MAX_FILE_CHUNKS: usize = 128;
/// Maximum chunks retained for documentation and prose files (.md, .txt, etc.).
/// Prevents sprawling changelogs or specifications from bloating the chunk pool
/// while preserving full coverage of file headers, setup, and key sections.
const MAX_DOC_FILE_CHUNKS: usize = 16;

/// What a cancelled search returns. Reaching this at all means the turn is being
/// stopped — the agent loop drops this tool's future on cancel anyway, so the
/// string only surfaces in the narrow window where cancel lands after the future
/// was polled but before the drop.
const CANCELLED: &str = "error: code_search was cancelled.";

/// BM25 free parameters (Okapi defaults; widely used, no tuning needed).
const K1: f64 = 1.5;
const B: f64 = 0.75;

/// Snippet lines shown per ranked chunk (and the per-line char cap), mirroring
/// project_search's snippet style so the two tools read as one family.
const SNIPPET_LINES: usize = 4;
const SNIPPET_CHARS: usize = 160;

/// Hard ceiling on result lines (headers + snippets) so a broad query can't blow
/// the model's context.
const OUT_LINE_CAP: usize = 80;

/// Reciprocal Rank Fusion constant (the standard k=60): each arm contributes
/// `1/(60 + rank)` to a chunk's fused score, so consensus across arms beats a
/// strong showing in only one.
const RRF_K: f64 = 60.0;

/// bge-en-v1.5 query instruction (from the model card): prepended to the QUERY
/// only; passages/chunks are embedded plain. fastembed-rs does NOT add this
/// itself, and omitting it measurably weakens retrieval.
const QUERY_PREFIX: &str = "Represent this sentence for searching relevant passages: ";

/// Identifies the embedding model and vector storage strategy whose vectors are
/// persisted. A mismatch (model swapped, or strategy updated) invalidates the
/// on-disk index cleanly so stale-dimension vectors are never reused.
const MODEL_KEY: &str = "bge-small-en-v1.5/two-stage-1";

/// Maximum candidate chunks forwarded from BM25 to the vector re-ranking arm.
/// Sized so CPU inference stays well under 400ms even on a completely cold cache
/// (bge-small embeds ~50-80 chunks/sec on typical modern laptop CPUs, and cached
/// chunks cost 0ms).
const CANDIDATE_BUDGET: usize = 200;

/// Minimum cosine similarity for the vector arm to count a chunk as relevant.
/// bge-small (with its query instruction) has a compressed, high range: even an
/// unrelated/gibberish query scores ~0.44-0.50 against code chunks, while a real
/// match sits ~0.52+. This floor drops the garbage without hiding real hits. It
/// is the tuning knob for the vector arm, and must be re-calibrated if the model
/// is swapped (see `embedder`). Pinned by the `hybrid_nonsense_query_is_an_honest_miss`
/// end-to-end test.
const VEC_MIN_COSINE: f32 = 0.51;

/// Maximum number of repository corpora retained in RAM across sessions/projects.
const MAX_CACHED_REPOS: usize = 8;

/// Version key for persisted corpus files; bumping invalidates older incompatible caches.
const CORPUS_VERSION: &str = "two-stage-corpus-v1";

/// One retrieval unit: a slice of a file, identified by path + inclusive 1-based
/// line range, with the text it covers.
#[derive(Clone, Serialize, Deserialize)]
struct Chunk {
    path: String,
    start: usize,
    end: usize,
    text: String,
}

/// A chunk with its precomputed BM25 statistics: token count (doc length) and
/// per-term frequency. Term counts are computed once per chunk, not per query.
/// `embeddable` routes test/noise chunks away from the vector arm (BM25 only).
#[derive(Clone, Serialize, Deserialize)]
struct IndexedChunk {
    chunk: Chunk,
    len: usize,
    tf: HashMap<String, usize>,
    embeddable: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct FileCorpusEntry {
    mtime_nanos: u128,
    size: u64,
    chunks: Vec<IndexedChunk>,
}

#[derive(Serialize, Deserialize)]
struct PersistedCorpus {
    version: String,
    files: HashMap<String, FileCorpusEntry>,
}

#[derive(Default)]
struct RepoCorpus {
    files: HashMap<String, FileCorpusEntry>,
    hydrated: bool,
}

struct CorpusCache {
    repos: HashMap<String, RepoCorpus>,
    order: VecDeque<String>,
}

impl CorpusCache {
    fn new() -> Self {
        Self {
            repos: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(pos) = self.order.iter().position(|k| k == key) {
            self.order.remove(pos);
        }
        if self.order.len() >= MAX_CACHED_REPOS {
            if let Some(oldest) = self.order.pop_front() {
                self.repos.remove(&oldest);
            }
        }
        self.order.push_back(key.to_string());
    }
}

static CORPUS_CACHE: OnceLock<Mutex<CorpusCache>> = OnceLock::new();

fn corpus_cache() -> &'static Mutex<CorpusCache> {
    CORPUS_CACHE.get_or_init(|| Mutex::new(CorpusCache::new()))
}

/// Identifier-aware tokenizer. Splits on non-alphanumeric boundaries (snake_case,
/// kebab-case, spaces, punctuation) AND on camelCase/PascalCase boundaries, then
/// lowercases. This is the single biggest correctness lever for code search:
/// without it the query "update scratchpad" and the identifier `update_scratchpad`
/// / `updateScratchpad` share no token, and a lexical search is just a worse
/// substring grep. Acronyms stay intact ("URLParser" → url + parser).
fn tokenize(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_alphanumeric() {
            let prev = if i > 0 { Some(chars[i - 1]) } else { None };
            let next = chars.get(i + 1).copied();
            // lower→Upper is a camelCase boundary; Upper→Upper followed by lower
            // is the acronym→word boundary ("URLParser" → "URL" | "Parser").
            let boundary = match prev {
                Some(p) => {
                    (p.is_lowercase() && c.is_uppercase())
                        || (p.is_uppercase()
                            && c.is_uppercase()
                            && matches!(next, Some(n) if n.is_lowercase()))
                }
                None => false,
            };
            if boundary && !cur.is_empty() {
                tokens.push(std::mem::take(&mut cur).to_lowercase());
            }
            cur.push(c);
        } else if !cur.is_empty() {
            tokens.push(std::mem::take(&mut cur).to_lowercase());
        }
    }
    if !cur.is_empty() {
        tokens.push(cur.to_lowercase());
    }
    tokens
}

/// Per-term frequency over one chunk's tokens.
fn term_freq(tokens: &[String]) -> HashMap<String, usize> {
    let mut m: HashMap<String, usize> = HashMap::new();
    for t in tokens {
        *m.entry(t.clone()).or_insert(0) += 1;
    }
    m
}

/// Split `content` into chunks of at most `token_budget` tokens (as counted by
/// `count`), always on line boundaries so chunk line numbers stay exact for
/// `project_read_lines`. A single line longer than the budget becomes its own
/// chunk (it will still truncate at embed time, but such lines are minified or
/// vendored and are excluded upstream anyway).
fn chunk_file(
    rel: &str,
    content: &str,
    token_budget: usize,
    count: &dyn Fn(&str) -> usize,
) -> Vec<Chunk> {
    let lines: Vec<&str> = content.lines().collect();
    let mut chunks = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let mut end = i; // 0-based exclusive == 1-based inclusive last line
        let mut acc = 0usize;
        while end < lines.len() {
            let lt = count(lines[end]).max(1);
            if end > i && acc + lt > token_budget {
                break;
            }
            acc += lt;
            end += 1;
        }
        chunks.push(Chunk {
            path: rel.to_string(),
            start: i + 1,
            end, // 1-based inclusive, equals the 0-based exclusive slice index
            text: lines[i..end].join("\n"),
        });
        i = end;
    }
    chunks
}

/// Where a symbol's chunk should begin: its own line, walked up over the doc
/// comment / attribute / decorator block directly above it, so documentation
/// travels with the item it describes instead of the previous chunk's tail.
/// Conservative heuristic (`//`, `/*`, `*`, `#`, `@`, Python docstrings) — a
/// stray match only shifts a boundary a line or two early; coverage and line
/// numbers are unaffected either way.
fn doc_attached_start(lines: &[&str], sym_line: usize) -> usize {
    if lines.is_empty() {
        return sym_line;
    }
    let mut start = sym_line.clamp(1, lines.len());
    while start > 1 {
        let t = lines[start - 2].trim_start();
        let commentish = t.starts_with("//")
            || t.starts_with("/*")
            || t.starts_with('*')
            || t.starts_with('#')
            || t.starts_with('@')
            || t.starts_with("\"\"\"")
            || t.starts_with("'''");
        if !commentish {
            break;
        }
        start -= 1;
    }
    start
}

/// AST-aware chunking for supported source files: cut at symbol boundaries
/// (functions, methods, type definitions, impl/class blocks — via the
/// Tree-Sitter symbol engine with internals included), then greedily merge
/// adjacent regions up to `token_budget` so small siblings share a chunk and
/// counts stay low. A single region that alone exceeds the budget (a 2,000-line
/// function) is subdivided by the plain line-based chunker, rebased to absolute
/// line numbers. Files without AST support — and code with no extractable
/// symbols — keep the blind windowing unchanged.
fn chunk_file_aware(
    rel: &str,
    content: &str,
    token_budget: usize,
    count: &dyn Fn(&str) -> usize,
) -> Vec<Chunk> {
    if !super::symbols::is_supported_code_file(rel) {
        return chunk_file(rel, content, token_budget, count);
    }
    let lines: Vec<&str> = content.lines().collect();
    let ext = Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    let mut cuts: Vec<usize> = super::symbols::extract_symbols_scoped(content, ext, true)
        .into_iter()
        .map(|s| doc_attached_start(&lines, s.line_no))
        .filter(|l| *l >= 1 && *l <= lines.len())
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    if cuts.is_empty() {
        return chunk_file(rel, content, token_budget, count);
    }
    // Regions: the line ranges (0-based, end-exclusive) between cut points,
    // starting with the file head (imports, module docs) before the first symbol.
    let mut regions: Vec<(usize, usize)> = Vec::with_capacity(cuts.len() + 1);
    let mut prev = 0usize;
    for cut in &cuts {
        if *cut - 1 > prev {
            regions.push((prev, cut - 1));
        }
        prev = cut - 1;
    }
    if prev < lines.len() {
        regions.push((prev, lines.len()));
    }
    // Greedy merge of regions up to the budget; one region over budget (a single
    // giant symbol) is subdivided along lines via the plain chunker.
    let line_cost: Vec<usize> = lines.iter().map(|l| count(l).max(1)).collect();
    let mut out: Vec<Chunk> = Vec::with_capacity(regions.len());
    let mut i = 0usize;
    while i < regions.len() {
        let (start, end) = regions[i];
        let mut acc: usize = line_cost[start..end].iter().sum();
        let mut j = i + 1;
        while j < regions.len() {
            let (s2, e2) = regions[j];
            let t2: usize = line_cost[s2..e2].iter().sum();
            if acc + t2 > token_budget {
                break;
            }
            acc += t2;
            j += 1;
        }
        if j == i + 1 && acc > token_budget {
            let text = lines[start..end].join("\n");
            for mut c in chunk_file(rel, &text, token_budget, count) {
                c.start += start;
                c.end += start;
                out.push(c);
            }
        } else {
            out.push(Chunk {
                path: rel.to_string(),
                start: start + 1,
                end,
                text: lines[start..end].join("\n"),
            });
        }
        i = j;
    }
    out
}

/// Merge adjacent chunks so no single file yields more than `cap` of them, for
/// files so large (generated, vendored, or just enormous) that the budget alone
/// explodes the batch count. Coverage is never dropped: merged chunks span the
/// exact combined line range — BM25 sees every token, and the vector arm just
/// embeds the leading ones (bge truncates at 512).
fn apply_file_cap(chunks: Vec<Chunk>, cap: usize) -> Vec<Chunk> {
    let n = chunks.len();
    if n <= cap {
        return chunks;
    }
    let group = n.div_ceil(cap.max(1));
    let mut out = Vec::with_capacity(cap);
    for part in chunks.chunks(group) {
        out.push(Chunk {
            path: part[0].path.clone(),
            start: part[0].start,
            end: part[part.len() - 1].end,
            text: part
                .iter()
                .map(|c| c.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        });
    }
    out
}

/// Generated manifests are excluded from the semantic/ranked corpus: they are
/// near-empty of intent (thousands of dependency-version lines) yet weakly match
/// any query about "version"/"install"/"build", so they crowd real hits out of
/// the top-k. `project_search` still covers them for literal lookups — this only
/// keeps them out of the *ranked* index.
fn is_generated_manifest(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    matches!(
        name,
        "Cargo.lock"
            | "package-lock.json"
            | "yarn.lock"
            | "pnpm-lock.yaml"
            | "npm-shrinkwrap.json"
            | "composer.lock"
            | "poetry.lock"
            | "Pipfile.lock"
            | "Gemfile.lock"
            | "go.sum"
            | "mix.lock"
            | "packages.lock.json"
    )
}

/// Vendored/generated paths excluded from the ranked corpus. These are files the
/// developer didn't author (third-party runtimes, generated schemas), so indexing
/// them pollutes ranking with semantically-near-empty text — the same reason
/// generated manifests are excluded. `project_search` still covers them for
/// literal lookups; this only keeps them out of the *ranked* index.
fn is_ranked_noise(rel: &str) -> bool {
    if is_generated_manifest(rel) {
        return true;
    }
    if rel.contains("/.fastembed_cache/")
        || rel.starts_with(".fastembed_cache/")
        || rel.contains("/pyodide/")
        || rel.starts_with("pyodide/")
        || rel.contains("/gen/schemas/")
        || rel.starts_with("gen/schemas/")
    {
        return true;
    }

    // Path segments indicating execution logs, telemetry dumps, or benchmark runs
    for seg in rel.split('/') {
        if matches!(
            seg,
            "logs"
                | "log"
                | "traces"
                | "dumps"
                | "pre_validation_runs"
                | ".turbo"
                | ".cache"
        ) {
            return true;
        }
    }

    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    name.ends_with(".log")
        || name.ends_with(".transcript.txt")
        || name.ends_with(".transcript")
        || name == "transcript.txt"
        || name.ends_with(".bak")
        || name.ends_with(".tmp")
}

/// Documentation and prose files (.md, .txt, .rst, .adoc).
/// Kept searchable, but capped in per-file chunking and in the candidate pool
/// so massive changelogs and specs do not starve source code from vector reranking.
fn is_doc_file(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.ends_with(".md")
        || lower.ends_with(".markdown")
        || lower.ends_with(".txt")
        || lower.ends_with(".rst")
        || lower.ends_with(".adoc")
}

/// Test/mock/fixture content: indexed for BM25 (gold for literal keyword
/// lookups) but never embedded — thousands of assertion lines and canned data
/// are exactly the vector cache's CPU bloat, and the semantic arm adds nothing
/// over keyword retrieval for them. Deliberately separate from `is_ranked_noise`
/// (which drops files from the corpus entirely): these must stay findable.
fn is_test_noise(rel: &str) -> bool {
    // Conventional test/mock/fixture directories at any depth.
    for seg in rel.split('/') {
        if matches!(
            seg,
            "tests"
                | "test"
                | "__tests__"
                | "__mocks__"
                | "__fixtures__"
                | "testdata"
                | "fixtures"
                | "mocks"
                | "__snapshots__"
        ) {
            return true;
        }
    }
    // Conventional test file names: foo.test.ts, foo.spec.tsx, foo_test.go,
    // test_foo.py, test.py, x.snap.
    let name = rel.rsplit('/').next().unwrap_or(rel).to_ascii_lowercase();
    if name.ends_with(".snap") {
        return true;
    }
    let stem = name.rsplit_once('.').map(|(s, _)| s).unwrap_or(name.as_str());
    name.contains(".test.")
        || name.contains(".spec.")
        || stem == "test"
        || stem.starts_with("test_")
        || stem.ends_with("_test")
}

/// First line of Rust's inline test region (1-based): `#[cfg(test)] mod tests`
/// conventionally holds the file's test tail, whose assertion lines are the same
/// vector-cache bloat as standalone test files — so chunks starting at or after
/// this line are built for BM25 only. `None` when the file has no test mod.
fn rust_test_tail_line(content: &str) -> Option<usize> {
    content
        .lines()
        .position(|l| {
            let t = l.trim_start();
            t.starts_with("#[cfg(test)]") || t.starts_with("#[cfg(all(test")
        })
        .map(|i| i + 1)
}

/// Stable (mtime_nanos, size) tuple used to validate file freshness in the corpus cache.
fn file_mtime_and_size(path: &Path) -> Option<(u128, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let nanos = mtime.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
    Some((nanos, meta.len()))
}

/// On-disk location of the persisted corpus cache: `app_data/search_index/<hash>.corpus.json`.
fn corpus_location(cache_dir: &Path, root: &Path) -> Option<(String, PathBuf)> {
    let canon = std::fs::canonicalize(root).ok()?;
    let key = canon.to_string_lossy().to_string();
    let hash = fnv1a64(key.as_bytes());
    Some((
        key,
        cache_dir
            .join("search_index")
            .join(format!("{hash:016x}.corpus.json")),
    ))
}

/// Load the persisted corpus index from disk if the version matches.
fn load_corpus(path: &Path) -> Option<HashMap<String, FileCorpusEntry>> {
    let bytes = std::fs::read(path).ok()?;
    let persisted: PersistedCorpus = serde_json::from_slice(&bytes).ok()?;
    if persisted.version != CORPUS_VERSION {
        return None;
    }
    Some(persisted.files)
}

/// Write the in-memory corpus cache to disk atomically.
fn persist_corpus(cache_dir: Option<&Path>, root: &Path, files: HashMap<String, FileCorpusEntry>) {
    let Some(dir) = cache_dir else { return };
    let Some((_key, path)) = corpus_location(dir, root) else { return };
    let persisted = PersistedCorpus {
        version: CORPUS_VERSION.to_string(),
        files,
    };
    let Ok(bytes) = serde_json::to_vec(&persisted) else { return };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::write(&tmp, &bytes);
    let _ = std::fs::rename(&tmp, &path);
}

/// Parse and chunk a single file for both lexical and semantic retrieval.
fn parse_file_corpus(
    root: &Path,
    rel: &str,
    with_vector: bool,
    counter: &dyn Fn(&str) -> usize,
) -> Option<Vec<IndexedChunk>> {
    let path = root.join(rel);
    let content = read_text_capped(&path).ok()?;
    let test_file = is_test_noise(rel);
    let test_from = if rel.to_ascii_lowercase().ends_with(".rs") {
        rust_test_tail_line(&content)
    } else {
        None
    };
    let raw = if with_vector && !test_file {
        chunk_file_aware(rel, &content, CHUNK_TOKEN_BUDGET, counter)
    } else {
        chunk_file(rel, &content, CHUNK_TOKEN_BUDGET, counter)
    };
    let cap = if is_doc_file(rel) {
        MAX_DOC_FILE_CHUNKS
    } else {
        MAX_FILE_CHUNKS
    };
    let mut chunks = Vec::new();
    for chunk in apply_file_cap(raw, cap) {
        let tokens = tokenize(&chunk.text);
        if tokens.is_empty() {
            continue;
        }
        let len = tokens.len();
        let tf = term_freq(&tokens);
        let embeddable = !test_file && test_from.map(|t| chunk.start < t).unwrap_or(true);
        chunks.push(IndexedChunk {
            chunk,
            len,
            tf,
            embeddable,
        });
    }
    Some(chunks)
}

/// Parse files in parallel across CPU worker threads using a work-stealing atomic queue.
fn parse_files_parallel(
    root: &Path,
    items: &[(String, u128, u64)],
    with_vector: bool,
    cancel: &CancellationToken,
) -> Vec<(String, u128, u64, Vec<IndexedChunk>)> {
    if items.is_empty() || cancel.is_cancelled() {
        return Vec::new();
    }

    let num_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(16);

    let next_idx = std::sync::atomic::AtomicUsize::new(0);
    let results: std::sync::Mutex<Vec<(String, u128, u64, Vec<IndexedChunk>)>> =
        std::sync::Mutex::new(Vec::with_capacity(items.len()));

    std::thread::scope(|s| {
        for _ in 0..num_threads {
            s.spawn(|| {
                let counter = |s: &str| tokenize(s).len().max(1);
                let mut local = Vec::new();
                loop {
                    if cancel.is_cancelled() {
                        break;
                    }
                    let idx = next_idx.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if idx >= items.len() {
                        break;
                    }
                    let (rel, mtime, size) = &items[idx];
                    if let Some(chunks) = parse_file_corpus(root, rel, with_vector, &counter) {
                        local.push((rel.clone(), *mtime, *size, chunks));
                    }
                }
                if !local.is_empty() {
                    if let Ok(mut lock) = results.lock() {
                        lock.extend(local);
                    }
                }
            });
        }
    });

    results.into_inner().unwrap_or_default()
}

/// Retrieve or build the indexed corpus using an in-memory and on-disk mtime cache.
/// Files whose (mtime, size) match the cache are returned in 0ms; missing or modified
/// files are parsed in parallel across CPU cores.
fn get_or_build_corpus(
    root: &Path,
    files: &[String],
    with_vector: bool,
    cache_dir: Option<&Path>,
    cancel: &CancellationToken,
) -> Vec<IndexedChunk> {
    if cancel.is_cancelled() || files.is_empty() {
        return Vec::new();
    }

    let canon_key = std::fs::canonicalize(root)
        .map(|c| c.to_string_lossy().to_string())
        .unwrap_or_else(|_| root.to_string_lossy().to_string());

    let mut stale_or_missing: Vec<(String, u128, u64)> = Vec::new();

    // 1. Check in-memory cache and detect stale/missing files
    {
        let mut cache = match corpus_cache().lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        cache.touch(&canon_key);
        let repo = cache.repos.entry(canon_key.clone()).or_default();

        if !repo.hydrated {
            if let Some(dir) = cache_dir {
                if let Some((_key, path)) = corpus_location(dir, root) {
                    if let Some(persisted) = load_corpus(&path) {
                        repo.files = persisted;
                    }
                }
            }
            repo.hydrated = true;
        }

        for rel in files {
            if is_ranked_noise(rel) {
                continue;
            }
            let full = root.join(rel);
            if let Some((mtime, size)) = file_mtime_and_size(&full) {
                if let Some(entry) = repo.files.get(rel) {
                    if entry.mtime_nanos == mtime && entry.size == size {
                        continue;
                    }
                }
                stale_or_missing.push((rel.clone(), mtime, size));
            } else {
                repo.files.remove(rel);
            }
        }
    }

    // 2. Parse stale or missing files in parallel outside the lock
    if !stale_or_missing.is_empty() {
        let parsed = parse_files_parallel(root, &stale_or_missing, with_vector, cancel);
        if cancel.is_cancelled() {
            return Vec::new();
        }

        let mut cache = match corpus_cache().lock() {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };
        let repo = cache.repos.entry(canon_key.clone()).or_default();
        for (rel, mtime, size, chunks) in parsed {
            repo.files.insert(
                rel,
                FileCorpusEntry {
                    mtime_nanos: mtime,
                    size,
                    chunks,
                },
            );
        }

        if let Some(dir) = cache_dir {
            let files_clone = repo.files.clone();
            let root_buf = root.to_path_buf();
            let dir_buf = dir.to_path_buf();
            std::thread::spawn(move || {
                persist_corpus(Some(&dir_buf), &root_buf, files_clone);
            });
        }
    }

    // 3. Assemble chunks in file-walk order
    let cache = match corpus_cache().lock() {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let Some(repo) = cache.repos.get(&canon_key) else {
        return Vec::new();
    };

    let mut indexed = Vec::new();
    for rel in files {
        if let Some(entry) = repo.files.get(rel) {
            indexed.extend(entry.chunks.iter().cloned());
        }
    }
    indexed
}

/// Walk + read + chunk the corpus for both lexical and semantic retrieval.
/// Retained for test compatibility; delegates to the incremental parallel engine.
#[allow(dead_code)]
fn build_corpus(
    root: &Path,
    files: &[String],
    with_vector: bool,
    cancel: &CancellationToken,
) -> Vec<IndexedChunk> {
    get_or_build_corpus(root, files, with_vector, None, cancel)
}

/// Pre-warms the parsed corpus cache in the background for a project root.
/// Walks project files, hydrates persisted cache from disk, and parses any
/// stale or missing files in parallel so subsequent `code_search` calls from
/// models are instant (< 50ms).
pub fn warm_cache(root: &Path, cache_dir: Option<&Path>) {
    let mut files = Vec::new();
    Tools::walk_project(root, root, &mut files, FILE_CAP);
    let cancel = CancellationToken::new();
    let _ = get_or_build_corpus(root, &files, true, cache_dir, &cancel);
}

/// Progress payload reported during background repository indexing.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexProgress {
    pub status: String,
    pub current: usize,
    pub total: usize,
    pub message: Option<String>,
}

/// Walks the project, builds the parsed corpus, and embeds all missing chunks
/// into the vector cache in the background. Emits progress reports via `on_progress`.
pub fn index_repository_full(
    root: &Path,
    cache_dir: Option<&Path>,
    cancel: &CancellationToken,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<(), String> {
    let mut files = Vec::new();
    Tools::walk_project(root, root, &mut files, FILE_CAP);
    if cancel.is_cancelled() {
        on_progress(IndexProgress {
            status: "cancelled".into(),
            current: 0,
            total: 0,
            message: Some("Indexing stopped.".into()),
        });
        return Ok(());
    }

    let indexed = get_or_build_corpus(root, &files, true, cache_dir, cancel);
    if cancel.is_cancelled() {
        on_progress(IndexProgress {
            status: "cancelled".into(),
            current: 0,
            total: 0,
            message: Some("Indexing stopped.".into()),
        });
        return Ok(());
    }

    let embeddable_chunks: Vec<(u64, String)> = indexed
        .into_iter()
        .filter(|ic| ic.embeddable)
        .map(|ic| {
            let h = fnv1a64(ic.chunk.text.as_bytes());
            (h, ic.chunk.text)
        })
        .collect();

    let total = embeddable_chunks.len();
    if total == 0 {
        let msg = "✓ No embeddable code chunks found in project.".to_string();
        on_progress(IndexProgress {
            status: "up_to_date".into(),
            current: 0,
            total: 0,
            message: Some(msg),
        });
        return Ok(());
    }

    // Hydrate vector cache and determine which chunks are missing
    let mut missing: Vec<(u64, String)> = Vec::new();
    let mut cached_count = 0;
    let mut seen_missing = HashSet::new();

    {
        let mut cache = content_cache()
            .lock()
            .map_err(|_| "content cache lock poisoned".to_string())?;
        hydrate_locked(&mut cache, root, cache_dir);
        for (h, text) in embeddable_chunks {
            if cache.get(h).is_some() {
                cached_count += 1;
            } else if seen_missing.insert(h) {
                missing.push((h, text));
            }
        }
    }

    if missing.is_empty() {
        let msg = format!("✓ Vector cache up to date — all {total} chunks ready.");
        on_progress(IndexProgress {
            status: "up_to_date".into(),
            current: total,
            total,
            message: Some(msg),
        });
        return Ok(());
    }

    on_progress(IndexProgress {
        status: "indexing".into(),
        current: cached_count,
        total,
        message: None,
    });

    let mut count = cached_count;
    let mut unpersisted_count = 0;
    for part in missing.chunks(EMBED_BATCH) {
        if cancel.is_cancelled() {
            let cache = content_cache()
                .lock()
                .map_err(|_| "content cache lock poisoned".to_string())?;
            persist_index(&cache, root, cache_dir);
            on_progress(IndexProgress {
                status: "cancelled".into(),
                current: count,
                total,
                message: Some(format!("Vector indexing stopped ({count} / {total} chunks cached).")),
            });
            return Ok(());
        }

        let texts: Vec<String> = part.iter().map(|(_, t)| t.clone()).collect();
        let embs = embed_texts(&texts)?;

        {
            let mut cache = content_cache()
                .lock()
                .map_err(|_| "content cache lock poisoned".to_string())?;
            for ((h, _), e) in part.iter().zip(embs) {
                cache.insert(*h, e);
            }
        }

        count += part.len();
        unpersisted_count += part.len();

        if unpersisted_count >= 256 {
            let cache = content_cache()
                .lock()
                .map_err(|_| "content cache lock poisoned".to_string())?;
            persist_index(&cache, root, cache_dir);
            unpersisted_count = 0;
        }

        on_progress(IndexProgress {
            status: "indexing".into(),
            current: count.min(total),
            total,
            message: None,
        });
    }

    // Final flush to disk
    {
        let cache = content_cache()
            .lock()
            .map_err(|_| "content cache lock poisoned".to_string())?;
        persist_index(&cache, root, cache_dir);
    }

    let msg = format!("✓ Vector cache complete — {total} chunks ready for instant search.");
    on_progress(IndexProgress {
        status: "done".into(),
        current: total,
        total,
        message: Some(msg),
    });

    Ok(())
}

/// Okapi BM25 score of one chunk against the (distinct) query terms.
fn bm25_score(
    ic: &IndexedChunk,
    query_terms: &[String],
    df: &HashMap<&str, usize>,
    n: usize,
    avgdl: f64,
) -> f64 {
    let dl = ic.len as f64;
    let mut score = 0.0;
    for t in query_terms {
        let Some(&dft) = df.get(t.as_str()) else { continue };
        let Some(&tf) = ic.tf.get(t) else { continue };
        // idf with the +1 guard so it's always positive (the plain BM25 idf goes
        // negative once a term appears in over half the corpus).
        let idf = (1.0 + (n as f64 - dft as f64 + 0.5) / (dft as f64 + 0.5)).ln();
        let tf = tf as f64;
        let denom = tf + K1 * (1.0 - B + B * (dl / avgdl));
        score += idf * (tf * (K1 + 1.0)) / denom;
    }
    score
}

/// The most relevant lines inside a ranked chunk, for the snippet. Lines are
/// scored by how many query terms they hit (ties break to the earliest line), so
/// the snippet is a pointer to *why* the chunk matched, not just its first line.
/// Line numbers are absolute (chunk.start + offset).
fn best_lines(chunk: &Chunk, query_terms: &[String]) -> Vec<(usize, String)> {
    let qset: HashSet<String> = query_terms.iter().cloned().collect();
    let mut scored: Vec<(usize, usize, String)> = Vec::new(); // (hits, line, text)
    for (i, line) in chunk.text.lines().enumerate() {
        let hits = tokenize(line)
            .into_iter()
            .filter(|t| qset.contains(t))
            .count();
        if hits > 0 {
            let mut txt = line.trim().to_string();
            if txt.chars().count() > SNIPPET_CHARS {
                txt = txt.chars().take(SNIPPET_CHARS).collect();
            }
            scored.push((hits, chunk.start + i, txt));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    scored.truncate(SNIPPET_LINES);
    scored.into_iter().map(|(_, ln, txt)| (ln, txt)).collect()
}

/// Snippet for a ranked chunk. Prefer the query-term-highlighted lines; when a
/// chunk matched purely semantically (no shared token), fall back to its opening
/// lines so the model still sees *what* matched, not just the path.
fn snippet_lines(chunk: &Chunk, query_terms: &[String]) -> Vec<(usize, String)> {
    let hits = best_lines(chunk, query_terms);
    if !hits.is_empty() {
        return hits;
    }
    chunk
        .text
        .lines()
        .take(SNIPPET_LINES)
        .enumerate()
        .map(|(i, line)| {
            let txt = line.trim().chars().take(SNIPPET_CHARS).collect::<String>();
            (chunk.start + i, txt)
        })
        .collect()
}

// ── vector arm ─────────────────────────────────────────

/// The embedding model, loaded lazily and cached. `Mutex` because `embed` takes
/// `&mut self`; the single user-facing search at a time means contention is nil.
/// If an initialization fails (e.g. offline when attempting first-time model download),
/// it returns an error for that call but does NOT lock in failure permanently —
/// subsequent calls will retry initialization.
static EMBEDDER: Mutex<Option<TextEmbedding>> = Mutex::new(None);

/// Status payload for the local semantic embedding model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmbeddingModelStatus {
    pub model_name: String,
    pub status: String,
    pub size_bytes: u64,
    pub size_formatted: String,
    pub cache_dir: String,
    pub is_loaded: bool,
    pub message: Option<String>,
}

static DOWNLOADING_EMBEDDER: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(meta) = entry.metadata() {
                if meta.is_dir() {
                    total += dir_size(&entry.path());
                } else {
                    total += meta.len();
                }
            }
        }
    }
    total
}

fn has_onnx_file(path: &Path) -> bool {
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    if has_onnx_file(&entry.path()) {
                        return true;
                    }
                } else if entry.path().extension().and_then(|s| s.to_str()) == Some("onnx") {
                    return true;
                }
            }
        }
    }
    false
}

fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".to_string();
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.2} GB", mb / 1024.0)
    } else {
        format!("{:.1} MB", mb)
    }
}

/// Resolves the model cache directory: FASTEMBED_CACHE_DIR override, or
/// app_data/models, or .fastembed_cache fallback.
pub fn resolve_model_cache_dir(app_data_dir: Option<&Path>) -> PathBuf {
    if let Ok(dir) = std::env::var("FASTEMBED_CACHE_DIR") {
        let p = PathBuf::from(dir.trim());
        if !p.as_os_str().is_empty() {
            return p;
        }
    }
    if let Some(dir) = app_data_dir {
        return dir.join("models");
    }
    PathBuf::from(".fastembed_cache")
}

/// Inspects the local cache directory to report model readiness, disk usage, and state.
pub fn embedding_model_status(app_data_dir: Option<&Path>) -> EmbeddingModelStatus {
    let cache_dir = resolve_model_cache_dir(app_data_dir);
    let model_folder = cache_dir.join("models--Xenova--bge-small-en-v1.5");
    let is_downloading = DOWNLOADING_EMBEDDER.load(std::sync::atomic::Ordering::SeqCst);
    let is_loaded = EMBEDDER.lock().map(|g| g.is_some()).unwrap_or(false);

    let (status, size_bytes, message) = if is_downloading {
        (
            "downloading".to_string(),
            dir_size(&model_folder),
            Some("Downloading model weights from Hugging Face...".to_string()),
        )
    } else if model_folder.exists() && has_onnx_file(&model_folder) {
        let sz = dir_size(&model_folder);
        if sz > 10_000_000 {
            ("ready".to_string(), sz, None)
        } else {
            (
                "not_downloaded".to_string(),
                sz,
                Some("Incomplete model files found.".to_string()),
            )
        }
    } else {
        (
            "not_downloaded".to_string(),
            0,
            Some("Model not cached locally. code_search will run BM25 lexical search only.".to_string()),
        )
    };

    EmbeddingModelStatus {
        model_name: "bge-small-en-v1.5".to_string(),
        status,
        size_bytes,
        size_formatted: format_size(size_bytes),
        cache_dir: cache_dir.to_string_lossy().to_string(),
        is_loaded,
        message,
    }
}

/// Downloads and initializes the bge-small model, executing a warm-up inference pass.
pub fn download_embedding_model(app_data_dir: Option<&Path>) -> Result<EmbeddingModelStatus, String> {
    use std::sync::atomic::Ordering;
    if DOWNLOADING_EMBEDDER.swap(true, Ordering::SeqCst) {
        return Err("A model download is already in progress.".into());
    }

    struct DownloadGuard;
    impl Drop for DownloadGuard {
        fn drop(&mut self) {
            DOWNLOADING_EMBEDDER.store(false, Ordering::SeqCst);
        }
    }
    let _guard = DownloadGuard;

    let cache_dir = resolve_model_cache_dir(app_data_dir);
    let _ = std::fs::create_dir_all(&cache_dir);

    let opts = fastembed::TextInitOptions::new(fastembed::EmbeddingModel::BGESmallENV15)
        .with_cache_dir(cache_dir)
        .with_show_download_progress(false);

    let mut model = TextEmbedding::try_new(opts).map_err(|e| format!("Failed to download/load model: {e}"))?;

    // Warm-up inference pass
    let _ = model
        .embed(vec!["Outpost embedding initialization warm-up".to_string()], None)
        .map_err(|e| format!("Model downloaded but warm-up inference failed: {e}"))?;

    if let Ok(mut guard) = EMBEDDER.lock() {
        *guard = Some(model);
    }

    drop(_guard);

    Ok(embedding_model_status(app_data_dir))
}

/// Deletes the cached model from disk and releases any loaded ONNX session in memory.
pub fn delete_embedding_model(app_data_dir: Option<&Path>) -> Result<EmbeddingModelStatus, String> {
    if let Ok(mut guard) = EMBEDDER.lock() {
        *guard = None;
    }

    let cache_dir = resolve_model_cache_dir(app_data_dir);
    let model_folder = cache_dir.join("models--Xenova--bge-small-en-v1.5");
    if model_folder.exists() {
        std::fs::remove_dir_all(&model_folder)
            .map_err(|e| format!("Failed to delete model files: {e}"))?;
    }

    Ok(embedding_model_status(app_data_dir))
}

fn with_embedder<R>(f: impl FnOnce(&mut TextEmbedding) -> Result<R, String>) -> Result<R, String> {
    let mut guard = EMBEDDER
        .lock()
        .map_err(|_| "embedder lock poisoned".to_string())?;
    if guard.is_none() {
        let cache_dir = resolve_model_cache_dir(None);
        let opts = fastembed::TextInitOptions::new(fastembed::EmbeddingModel::BGESmallENV15)
            .with_cache_dir(cache_dir)
            .with_show_download_progress(false);
        let model = TextEmbedding::try_new(opts).map_err(|e| e.to_string())?;
        *guard = Some(model);
    }
    let model = guard.as_mut().expect("embedder initialized above");
    f(model)
}


/// Embed a batch of texts in one inference call.
fn embed_texts(texts: &[String]) -> Result<Vec<Vec<f32>>, String> {
    with_embedder(|model| model.embed(texts, None).map_err(|e| e.to_string()))
}

/// Embed the query with the bge query instruction prepended.
fn embed_query(query: &str) -> Result<Vec<f32>, String> {
    let text = format!("{QUERY_PREFIX}{query}");
    embed_texts(&[text])?
        .into_iter()
        .next()
        .ok_or_else(|| "embedder returned no query vector".to_string())
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let na: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let nb: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// The on-disk representation of cached chunk embeddings.
/// Stored as a flat list of (chunk_hash, embedding) pairs.
#[derive(Serialize, Deserialize)]
struct PersistedIndex {
    model: String,
    vectors: Vec<(u64, Vec<f32>)>,
}

/// Maximum number of chunk vectors kept in the in-memory content cache.
/// 50,000 chunks × 384 floats × 4 bytes ≈ 76MB of RAM.
const CONTENT_CACHE_CAPACITY: usize = 50000;

/// Bounded in-memory content-addressable cache of chunk embeddings.
/// Key is a stable 64-bit hash (FNV-1a) of the chunk's exact text.
struct ContentVectorCache {
    entries: HashMap<u64, Vec<f32>>,
    order: VecDeque<u64>,
    loaded_root: Option<String>,
}

impl ContentVectorCache {
    fn new() -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            loaded_root: None,
        }
    }

    fn contains(&self, hash: u64) -> bool {
        self.entries.contains_key(&hash)
    }

    fn get(&mut self, hash: u64) -> Option<&Vec<f32>> {
        if self.entries.contains_key(&hash) {
            if let Some(pos) = self.order.iter().position(|&h| h == hash) {
                self.order.remove(pos);
            }
            self.order.push_back(hash);
            self.entries.get(&hash)
        } else {
            None
        }
    }

    fn insert(&mut self, hash: u64, vec: Vec<f32>) {
        if self.entries.contains_key(&hash) {
            return;
        }
        if self.entries.len() >= CONTENT_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
        self.order.push_back(hash);
        self.entries.insert(hash, vec);
    }
}

static CONTENT_CACHE: OnceLock<Mutex<ContentVectorCache>> = OnceLock::new();

fn content_cache() -> &'static Mutex<ContentVectorCache> {
    CONTENT_CACHE.get_or_init(|| Mutex::new(ContentVectorCache::new()))
}

/// FNV-1a 64-bit hash — stable across runs and compilers, used to hash chunk text
/// for content-addressable caching and to name per-repo index files.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Where this repo's persisted index lives: `app_data/search_index/<hash>.json`,
/// with `<hash>` a stable digest of the canonicalized root path. Returns `None`
/// when persistence is disabled or the root can't be canonicalized (e.g. deleted
/// mid-search), in which case search falls back to the in-process cache only.
fn index_location(cache_dir: &Path, root: &Path) -> Option<(String, PathBuf)> {
    let canon = std::fs::canonicalize(root).ok()?;
    let key = canon.to_string_lossy().to_string();
    let hash = fnv1a64(key.as_bytes());
    Some((
        key,
        cache_dir
            .join("search_index")
            .join(format!("{hash:016x}.json")),
    ))
}

/// Read + parse a persisted index, returning its vectors iff the model key matches
/// (so a swapped model never reuses stale-dimension vectors).
fn load_index(path: &Path) -> Option<Vec<(u64, Vec<f32>)>> {
    let bytes = std::fs::read(path).ok()?;
    let persisted: PersistedIndex = serde_json::from_slice(&bytes).ok()?;
    if persisted.model != MODEL_KEY {
        return None;
    }
    Some(persisted.vectors)
}

/// Hydrate the in-memory cache from the persisted index, once per repo per
/// process (`loaded_root` tracks which repo the files were loaded for). Called
/// under the cache lock; never fails the search — a missing/corrupt index just
/// leaves the cache empty for a clean rebuild.
fn hydrate_locked(cache: &mut ContentVectorCache, root: &Path, cache_dir: Option<&Path>) {
    let Some(dir) = cache_dir else { return };
    let Some((key, path)) = index_location(dir, root) else {
        return;
    };
    if cache.loaded_root.as_deref() == Some(key.as_str()) {
        return; // already hydrated for this repo this process
    }
    cache.entries.clear();
    cache.order.clear();
    if let Some(vectors) = load_index(&path) {
        for (h, vec) in vectors {
            cache.insert(h, vec);
        }
    }
    cache.loaded_root = Some(key);
}

/// Write the in-memory cache to disk (best-effort: a failed write just means the
/// next process re-embeds). Written atomically (tmp + rename) so a crash mid-write
/// can't corrupt the index.
fn persist_index(cache: &ContentVectorCache, root: &Path, cache_dir: Option<&Path>) {
    let Some(dir) = cache_dir else { return };
    let Some((_key, path)) = index_location(dir, root) else {
        return;
    };
    let vectors: Vec<(u64, Vec<f32>)> = cache
        .order
        .iter()
        .filter_map(|h| cache.entries.get(h).map(|v| (*h, v.clone())))
        .collect();
    let persisted = PersistedIndex {
        model: MODEL_KEY.to_string(),
        vectors,
    };
    let Ok(bytes) = serde_json::to_vec(&persisted) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    let _ = std::fs::write(&tmp, &bytes);
    let _ = std::fs::rename(&tmp, &path);
}

/// Two-Stage vector re-ranking (Paradigm A): dynamically embed and score *only*
/// the candidate chunks nominated by BM25 (or the small-repo candidate pool).
/// Cached chunks cost 0ms inference; missing candidates are embedded in
/// EMBED_BATCH-sized batches with cancel checkpoints.
fn vector_ranked(
    indexed: &[IndexedChunk],
    candidate_indices: &[usize],
    query: &str,
    root: &Path,
    cache_dir: Option<&Path>,
    cancel: &CancellationToken,
) -> Result<Vec<(f32, usize)>, String> {
    if candidate_indices.is_empty() {
        return Ok(Vec::new());
    }
    if cancel.is_cancelled() {
        return Err("cancelled".into());
    }

    // 1. Identify which candidate chunks need embedding vs which are in cache.
    let mut missing_texts: Vec<(usize, u64, String)> = Vec::new();
    let mut candidate_vectors: Vec<(usize, Vec<f32>)> = Vec::with_capacity(candidate_indices.len());

    {
        let mut cache = content_cache()
            .lock()
            .map_err(|_| "content cache lock poisoned".to_string())?;
        hydrate_locked(&mut cache, root, cache_dir);

        for &idx in candidate_indices {
            let text = &indexed[idx].chunk.text;
            let h = fnv1a64(text.as_bytes());
            if let Some(vec) = cache.get(h) {
                candidate_vectors.push((idx, vec.clone()));
            } else {
                missing_texts.push((idx, h, text.clone()));
            }
        }
    }

    // 2. Embed missing candidate chunks in EMBED_BATCH-sized batches with cancel checkpoints.
    let had_missing = !missing_texts.is_empty();
    if had_missing {
        let mut newly_embedded: Vec<(usize, u64, Vec<f32>)> = Vec::with_capacity(missing_texts.len());
        for part in missing_texts.chunks(EMBED_BATCH) {
            if cancel.is_cancelled() {
                return Err("cancelled".into());
            }
            let texts: Vec<String> = part.iter().map(|(_, _, t)| t.clone()).collect();
            let embs = embed_texts(&texts)?;
            for ((idx, h, _), e) in part.iter().zip(embs) {
                newly_embedded.push((*idx, *h, e));
            }
        }

        // Store back into cache under lock
        let mut cache = content_cache()
            .lock()
            .map_err(|_| "content cache lock poisoned".to_string())?;
        for (idx, h, e) in newly_embedded {
            cache.insert(h, e.clone());
            candidate_vectors.push((idx, e));
        }
        persist_index(&cache, root, cache_dir);
    }

    // 3. Embed the query with the bge instruction and rank candidates by cosine similarity.
    let q = embed_query(query)?;
    let mut scored: Vec<(f32, usize)> = candidate_vectors
        .into_iter()
        .map(|(idx, e)| (cosine(&q, &e), idx))
        .filter(|(s, _)| *s > VEC_MIN_COSINE)
        .collect();

    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    Ok(scored)
}

/// Reciprocal Rank Fusion: merge the BM25 ranking and the vector ranking into one
/// ordering. Each list contributes `1/(k + rank)`; a chunk absent from a list
/// (e.g. zero BM25 score, or below the vector floor) contributes nothing there.
fn rrf_merge(bm25_ranked: &[(f64, usize)], vec_ranked: &[(f32, usize)]) -> Vec<(f64, usize)> {
    let mut rrf: HashMap<usize, f64> = HashMap::new();
    for (rank, &(_, idx)) in bm25_ranked.iter().enumerate() {
        *rrf.entry(idx).or_insert(0.0) += 1.0 / (RRF_K + rank as f64);
    }
    for (rank, &(_, idx)) in vec_ranked.iter().enumerate() {
        *rrf.entry(idx).or_insert(0.0) += 1.0 / (RRF_K + rank as f64);
    }
    let mut merged: Vec<(f64, usize)> = rrf.into_iter().map(|(idx, s)| (s, idx)).collect();
    merged.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    merged
}

/// `code_search` — relevance-ranked project search (spec §9). Hybrid core;
/// called from `Tools::code_search` after the project-root guard, on a blocking
/// thread. `with_vector` lets tests exercise the BM25-only path without loading
/// the model.
pub fn search(
    arguments: &str,
    root: &Path,
    cache_dir: Option<&Path>,
    cancel: &CancellationToken,
) -> String {
    search_with(arguments, root, true, cache_dir, cancel)
}

fn search_with(
    arguments: &str,
    root: &Path,
    with_vector: bool,
    cache_dir: Option<&Path>,
    cancel: &CancellationToken,
) -> String {
    // Cancel checkpoints: entry (a Stop that landed while the tool was queued),
    // after the corpus walk (build_corpus returns partial on cancel), and before
    // the vector arm (whose own batch loop checkpoints deeper still).
    if cancel.is_cancelled() {
        return CANCELLED.into();
    }
    let parsed = serde_json::from_str::<Value>(arguments).ok();
    let query = parsed
        .as_ref()
        .and_then(|v| v["query"].as_str().map(str::to_string))
        .filter(|q| !q.trim().is_empty());
    let Some(query) = query else {
        return "error: code_search requires a non-empty 'query' string.".into();
    };
    let max_results = parsed
        .as_ref()
        .and_then(|v| v["max_results"].as_u64())
        .map(|n| n as usize)
        .unwrap_or(10)
        .clamp(1, 50);

    let path_arg = parsed
        .as_ref()
        .and_then(|v| v["path"].as_str())
        .map(str::to_string)
        .filter(|p| !p.trim().is_empty());

    let glob_arg = parsed
        .as_ref()
        .and_then(|v| v["glob"].as_str())
        .map(str::to_string)
        .filter(|g| !g.trim().is_empty());

    let glob_norm = glob_arg.as_deref().map(|g| g.replace('\\', "/"));
    let glob_pat = match glob_norm.as_deref() {
        Some(g) => match glob::Pattern::new(g) {
            Ok(p) => Some(p),
            Err(e) => return format!("error: invalid glob '{g}': {e}"),
        },
        None => None,
    };

    let path_norm = path_arg.as_deref().and_then(|p| {
        let trimmed = p.trim();
        if trimmed.is_empty() || trimmed == "." {
            return None;
        }
        match Tools::project_path(root, trimmed) {
            Ok(resolved) => {
                if let Ok(rel) = resolved.strip_prefix(root) {
                    let rel_str = rel.to_string_lossy().replace('\\', "/").trim_matches('/').to_string();
                    if rel_str.is_empty() || rel_str == "." {
                        None
                    } else {
                        Some(rel_str)
                    }
                } else {
                    Some(trimmed.replace('\\', "/").trim_matches('/').to_string())
                }
            }
            Err(_) => Some(trimmed.replace('\\', "/").trim_matches('/').to_string()),
        }
    });

    // Walk the project (same exclusions as project_search: node_modules, target,
    // dist, .git, …) and build the chunk corpus once for both arms.
    let mut files: Vec<String> = Vec::new();
    Tools::walk_project(root, root, &mut files, FILE_CAP);
    let initial_count = files.len();

    let mut path_matched_any = true;
    if let Some(ref p) = path_norm {
        let count_before = files.len();
        files.retain(|rel| rel == p || rel.starts_with(&format!("{p}/")));
        if files.is_empty() && count_before > 0 {
            path_matched_any = false;
        }
    }
    let mut glob_matched_any = true;
    if let Some(ref pat) = glob_pat {
        let count_before = files.len();
        let raw = glob_norm.as_deref().unwrap_or("");
        files.retain(|rel| {
            pat.matches(rel)
                || (!raw.contains('/')
                    && Path::new(rel)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .map(|n| pat.matches(n))
                        .unwrap_or(false))
        });
        if files.is_empty() && count_before > 0 {
            glob_matched_any = false;
        }
    }

    if files.is_empty() && initial_count > 0 {
        if !path_matched_any {
            if let Some(p) = path_arg {
                return format!(
                    "Path '{p}' matched no files, so nothing could be searched. Check the path or drop it."
                );
            }
        }
        if !glob_matched_any {
            if let Some(g) = glob_arg {
                return format!(
                    "Glob '{g}' matched no files, so nothing could be searched. Try a looser glob (e.g. '*.ts', 'src/**/*.rs') or drop it."
                );
            }
        }
        if let Some(p) = path_arg {
            return format!(
                "Path '{p}' matched no files, so nothing could be searched. Check the path or drop it."
            );
        }
        if let Some(g) = glob_arg {
            return format!(
                "Glob '{g}' matched no files, so nothing could be searched. Try a looser glob (e.g. '*.ts', 'src/**/*.rs') or drop it."
            );
        }
    }

    let indexed = get_or_build_corpus(root, &files, with_vector, cache_dir, cancel);
    if cancel.is_cancelled() {
        return CANCELLED.into();
    }
    if indexed.is_empty() {
        return format!("No searchable text found for '{query}'.");
    }

    // Corpus-wide term statistics, then the distinct query terms (repetitions
    // in the query shouldn't double-weight a term).
    let n = indexed.len();
    let mut df: HashMap<&str, usize> = HashMap::new();
    for ic in &indexed {
        for t in ic.tf.keys() {
            *df.entry(t.as_str()).or_insert(0) += 1;
        }
    }
    let total_tokens: usize = indexed.iter().map(|ic| ic.len).sum();
    let avgdl = total_tokens as f64 / n as f64;
    let mut seen: HashSet<String> = HashSet::new();
    let query_terms: Vec<String> = tokenize(&query)
        .into_iter()
        .filter(|t| seen.insert(t.clone()))
        .collect();
    if query_terms.is_empty() {
        return "error: query has no searchable terms.".into();
    }

    // BM25 arm: score every chunk; keep only positive scores (a chunk that shares
    // no query term with the query is irrelevant regardless of term commonness).
    let mut bm25_ranked: Vec<(f64, usize)> = Vec::new();
    for (i, ic) in indexed.iter().enumerate() {
        let s = bm25_score(ic, &query_terms, &df, n, avgdl);
        if s > 0.0 {
            bm25_ranked.push((s, i));
        }
    }
    bm25_ranked.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| indexed[a.1].chunk.path.cmp(&indexed[b.1].chunk.path))
            .then_with(|| indexed[a.1].chunk.start.cmp(&indexed[b.1].chunk.start))
    });

    // Two-Stage Candidate Selection:
    // Gather all embeddable chunks across the corpus (excluding test noise and inline test tails)
    let embeddable_indices: Vec<usize> = indexed
        .iter()
        .enumerate()
        .filter_map(|(i, ic)| if ic.embeddable { Some(i) } else { None })
        .collect();

    let candidate_indices: Vec<usize> = if with_vector {
        // Partition embeddable chunks into warm (already in vector cache, 0ms inference)
        // and cold (uncached, requires on-demand CPU embedding).
        let (mut warm_candidates, cold_set): (Vec<usize>, HashSet<usize>) = {
            let mut cache = content_cache().lock().ok();
            if let Some(ref mut c) = cache {
                hydrate_locked(c, root, cache_dir);
            }
            let mut warm = Vec::new();
            let mut cold = HashSet::new();
            for &idx in &embeddable_indices {
                let h = fnv1a64(indexed[idx].chunk.text.as_bytes());
                if cache.as_ref().map(|c| c.contains(h)).unwrap_or(false) {
                    warm.push(idx);
                } else {
                    cold.insert(idx);
                }
            }
            (warm, cold)
        };

        // If cold chunks are few (small repo, unit tests, or an indexed repo where
        // only a few files were written/edited), embed all of them on the fly!
        // Full semantic coverage across 100% of the codebase is preserved with no cliff.
        if cold_set.len() <= CANDIDATE_BUDGET {
            warm_candidates.extend(cold_set);
            warm_candidates
        } else {
            // Large un-indexed repo with many cold chunks: BM25 acts as the bouncer
            // to select at most CANDIDATE_BUDGET cold chunks to embed on-demand.
            // Documentation files (.md, .txt) can claim at most MAX_DOC_CANDIDATES slots.
            const MAX_DOC_CANDIDATES: usize = 50;
            let mut selected_cold = Vec::with_capacity(CANDIDATE_BUDGET);
            let mut doc_count = 0usize;
            let mut deferred_docs = Vec::new();

            for &(_, idx) in &bm25_ranked {
                if !cold_set.contains(&idx) {
                    continue;
                }
                if is_doc_file(&indexed[idx].chunk.path) {
                    if doc_count < MAX_DOC_CANDIDATES {
                        selected_cold.push(idx);
                        doc_count += 1;
                    } else {
                        deferred_docs.push(idx);
                    }
                } else {
                    selected_cold.push(idx);
                }
                if selected_cold.len() >= CANDIDATE_BUDGET {
                    break;
                }
            }

            if selected_cold.len() < CANDIDATE_BUDGET {
                for idx in deferred_docs {
                    selected_cold.push(idx);
                    if selected_cold.len() >= CANDIDATE_BUDGET {
                        break;
                    }
                }
            }

            warm_candidates.extend(selected_cold);
            warm_candidates
        }
    } else {
        // BM25-only mode (tests / model unavailable): bouncer nominates top BM25 candidates
        bm25_ranked
            .iter()
            .map(|&(_, idx)| idx)
            .filter(|&idx| indexed[idx].embeddable)
            .take(CANDIDATE_BUDGET)
            .collect()
    };

    // Vector arm: dynamically re-rank candidate chunks by cosine similarity;
    // on failure (model not downloaded / can't load) degrade to BM25-only with an honest note.
    // Cancellation is deliberately NOT treated as a degradable failure: falling
    // back to BM25 here would keep searching on a turn that is being stopped.
    if cancel.is_cancelled() {
        return CANCELLED.into();
    }
    let (final_ranked, arm_note): (Vec<(f64, usize)>, Option<String>) = if with_vector {
        if candidate_indices.is_empty() && bm25_ranked.is_empty() {
            (Vec::new(), None)
        } else {
            match vector_ranked(&indexed, &candidate_indices, &query, root, cache_dir, cancel) {
                Ok(vec_ranked) => (rrf_merge(&bm25_ranked, &vec_ranked), None),
                Err(_) if cancel.is_cancelled() => return CANCELLED.into(),
                Err(e) => (
                    bm25_ranked.clone(),
                    Some(format!(
                        "vector arm unavailable — {e}; results are BM25-only"
                    )),
                ),
            }
        }
    } else {
        (bm25_ranked.clone(), None)
    };

    if final_ranked.is_empty() {
        let hint = if glob_arg.is_some() || path_arg.is_some() {
            " Try looser filters, different wording, or use grep_search (or project_search) for a literal substring match."
        } else {
            " Try different wording, or use grep_search (or project_search) for a literal substring match."
        };
        let tip = if query_terms.len() == 1 {
            format!("\n[Tip: '{query}' is a single word. code_search works best with multi-word keywords or concepts (e.g. '{query} handler'). For exact symbol or keyword matches, use grep_search.]")
        } else {
            String::new()
        };
        return format!("No relevant matches for '{query}'.{hint}{tip}");
    }

    let top_score = final_ranked.first().map(|(s, _)| *s).unwrap_or(0.0);
    let mut shown = final_ranked.len().min(max_results);
    let allow_deep_tail = parsed.as_ref().and_then(|v| v.get("max_results")).is_some() && max_results > 10;
    let mut score_cliff_applied = false;
    if top_score > 0.0 && shown > 1 && !allow_deep_tail {
        let cutoff = top_score * 0.35;
        let mut filtered_len = 0;
        for (i, &(s, _)) in final_ranked.iter().take(shown).enumerate() {
            if i < 1 || s >= cutoff {
                filtered_len += 1;
            } else {
                break;
            }
        }
        if filtered_len < shown {
            shown = filtered_len;
            score_cliff_applied = true;
        }
    }

    let mut out: Vec<String> = Vec::new();
    let mut line_count = 0usize;
    for &(_, idx) in &final_ranked[..shown] {
        let chunk = &indexed[idx].chunk;
        let range = if chunk.start == chunk.end {
            format!("{}", chunk.start)
        } else {
            format!("{}-{}", chunk.start, chunk.end)
        };
        if line_count + 1 > OUT_LINE_CAP {
            break;
        }
        out.push(format!("{} ({range})", chunk.path));
        line_count += 1;
        for (ln, txt) in snippet_lines(chunk, &query_terms) {
            if line_count + 1 > OUT_LINE_CAP {
                break;
            }
            out.push(format!("  {ln}: {txt}"));
            line_count += 1;
        }
    }
    let mut s = out.join("\n");
    if final_ranked.len() > shown {
        if score_cliff_applied {
            s.push_str(&format!(
                "\n… ({} matches found; showing the top {shown} above relevance threshold)",
                final_ranked.len()
            ));
        } else {
            s.push_str(&format!(
                "\n… ({} relevant chunks; showing the top {shown})",
                final_ranked.len()
            ));
        }
    }
    if let Some(note) = arm_note {
        s.push_str(&format!("\n({note})"));
    }
    if query_terms.len() == 1 {
        s.push_str(&format!(
            "\n[Tip: '{query}' is a single word. code_search works best with multi-word keywords or concepts (e.g. '{query} handler'). For exact symbol or keyword matches, use grep_search.]"
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::{cosine, rrf_merge, search_with, tokenize, CancellationToken};
    use std::fs;
    use std::path::{Path, PathBuf};

    /// A throwaway project tree under the OS temp dir, removed on drop (same
    /// pattern as the project_search tests).
    struct Proj(PathBuf);

    impl Proj {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("outpost_code_search_{name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Proj(dir)
        }

        fn write(&self, rel: &str, body: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, body).unwrap();
        }
    }

    impl Drop for Proj {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    // BM25-only for the fast, hermetic suite (no model load / download).
    fn code_search(args: &str, root: &Path) -> String {
        search_with(args, root, false, None, &CancellationToken::new())
    }

    // A token cancelled before the search starts must stop it at the entry
    // checkpoint — no walk, no scoring — and report honestly as cancelled
    // rather than as "no matches" (which would read like a search miss).
    #[test]
    fn pre_cancelled_token_stops_search_at_entry() {
        let p = Proj::new("pre_cancelled");
        p.write("src/lib.rs", "fn alpha() {}\n");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = search_with(
            r#"{"query":"alpha"}"#,
            p.0.as_path(),
            false,
            None,
            &cancel,
        );
        assert_eq!(out, super::CANCELLED);
    }

    // build_corpus honors the token per file: a cancelled walk returns a partial
    // (here: empty) corpus, which the caller then abandons at its post-walk
    // checkpoint before any ranking runs.
    #[test]
    fn build_corpus_stops_early_on_cancel() {
        let p = Proj::new("corpus_cancel");
        p.write("src/a.rs", "fn a() {}\n");
        let files = vec!["src/a.rs".to_string()];
        let cancel = CancellationToken::new();
        cancel.cancel();
        let indexed = super::build_corpus(p.0.as_path(), &files, false, &cancel);
        assert!(indexed.is_empty(), "no chunks after cancel");
    }

    #[test]
    fn tokenizer_splits_camel_and_snake() {
        assert_eq!(
            tokenize("updateScratchpad"),
            vec!["update".to_string(), "scratchpad".to_string()]
        );
        assert_eq!(
            tokenize("update_scratchpad"),
            vec!["update".to_string(), "scratchpad".to_string()]
        );
        assert_eq!(
            tokenize("URLParser"),
            vec!["url".to_string(), "parser".to_string()]
        );
        assert_eq!(
            tokenize("foo bar-baz"),
            vec!["foo".to_string(), "bar".to_string(), "baz".to_string()]
        );
        assert_eq!(tokenize("ALPHA"), vec!["alpha".to_string()]);
    }

    #[test]
    fn cosine_identical_and_orthogonal() {
        assert!((cosine(&[1.0, 2.0], &[1.0, 2.0]) - 1.0).abs() < 1e-6);
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn rrf_prefers_consensus_across_arms() {
        // Chunk 0 is top in both lists → must win; chunk 2 is vector-only and
        // chunk 1 splits the difference, so 1 (two lower ranks) beats 2.
        let bm25 = vec![(5.0f64, 0usize), (4.0, 1)];
        let vec = vec![(0.9f32, 0usize), (0.8, 2), (0.7, 1)];
        let merged = rrf_merge(&bm25, &vec);
        assert_eq!(merged[0].1, 0, "consensus chunk first: {merged:?}");
        assert!(merged[1].1 == 1 && merged[2].1 == 2, "order: {merged:?}");
    }

    #[test]
    fn finds_identifier_from_spaced_query() {
        let p = Proj::new("id");
        p.write("src/lib.rs", "fn update_scratchpad() {}\nfn other() {}\n");
        let out = code_search(r#"{"query":"update scratchpad"}"#, p.0.as_path());
        assert!(
            out.contains("src/lib.rs"),
            "identifier matched by spaced query:\n{out}"
        );
    }

    #[test]
    fn ranks_more_relevant_first() {
        let p = Proj::new("rank");
        p.write(
            "src/a.rs",
            "fn alpha() {}\nfn alpha_two() {}\nfn alpha_three() {}\n",
        );
        p.write("src/b.rs", "fn alpha() {}\n");
        let out = code_search(r#"{"query":"alpha"}"#, p.0.as_path());
        let a = out.find("src/a.rs").expect("a.rs present");
        let b = out.find("src/b.rs").expect("b.rs present");
        assert!(a < b, "more hits ranks first:\n{out}");
    }

    #[test]
    fn reports_absolute_line_numbers_that_match_read_lines() {
        let p = Proj::new("range");
        // The target is the file's last line. The snippet line must be its
        // absolute 1-based number (251), matching project_read_lines' numbering,
        // regardless of where token-aware chunk boundaries fall.
        let mut body = String::new();
        for i in 0..250 {
            body.push_str(&format!("line {i}\n"));
        }
        body.push_str("fn target() {}\n");
        p.write("big.rs", &body);
        let out = code_search(r#"{"query":"target"}"#, p.0.as_path());
        assert!(out.contains("big.rs ("), "path + range:\n{out}");
        assert!(
            out.contains("251: fn target"),
            "absolute snippet line:\n{out}"
        );
    }

    #[test]
    fn empty_query_errors() {
        let p = Proj::new("empty");
        p.write("a.rs", "fn x() {}\n");
        for bad in [r#"{"query":"   "}"#, "{}", "not json"] {
            let out = code_search(bad, p.0.as_path());
            assert!(out.starts_with("error:"), "must error for {bad:?}:\n{out}");
        }
    }

    #[test]
    fn no_matches_is_honest_and_points_at_project_search() {
        let p = Proj::new("miss");
        p.write("a.rs", "fn x() {}\n");
        let out = code_search(r#"{"query":"zzz_nonexistent"}"#, p.0.as_path());
        assert!(
            out.starts_with("No relevant matches"),
            "honest miss:\n{out}"
        );
        assert!(
            out.contains("project_search"),
            "points at the literal tool:\n{out}"
        );
    }

    #[test]
    fn single_word_query_emits_guidance_tip() {
        let p = Proj::new("single_word_tip");
        p.write("a.rs", "fn sandbox_mode() {}\n");

        // 1. Single-word query with matches contains guidance tip
        let out_hit = code_search(r#"{"query":"sandbox"}"#, p.0.as_path());
        assert!(out_hit.contains("a.rs"), "hit present:\n{out_hit}");
        assert!(
            out_hit.contains("[Tip: 'sandbox' is a single word."),
            "tip should appear for single word hit:\n{out_hit}"
        );

        // 2. Multi-word query does NOT contain the single-word guidance tip
        let out_multi = code_search(r#"{"query":"how sandbox mode works"}"#, p.0.as_path());
        assert!(
            !out_multi.contains("[Tip:"),
            "multi-word query should not have single-word tip:\n{out_multi}"
        );

        // 3. Single-word query with no matches also includes the tip
        let out_miss = code_search(r#"{"query":"isolation"}"#, p.0.as_path());
        assert!(
            out_miss.contains("[Tip: 'isolation' is a single word."),
            "tip should appear for single word miss:\n{out_miss}"
        );
    }

    #[test]
    fn max_results_caps_output_and_reports() {
        let p = Proj::new("cap");
        for i in 0..5 {
            p.write(&format!("f{i}.rs"), "fn needle() {}\n");
        }
        let out = code_search(r#"{"query":"needle","max_results":2}"#, p.0.as_path());
        assert!(out.contains("showing the top 2"), "cap note:\n{out}");
    }

    #[test]
    fn score_cliff_suppresses_trailing_low_relevance_chunks() {
        let p = Proj::new("score_cliff");
        p.write("auth.rs", "fn authenticate_user_token() {\n    verify_token();\n}\n");
        for i in 0..5 {
            let mut filler = String::new();
            for j in 0..20 {
                filler.push_str(&format!("    let var_{j} = {j};\n"));
            }
            p.write(&format!("filler{i}.rs"), &format!("fn filler_{i}() {{\n    let token = 123;\n{filler}}}\n"));
        }
        let out = code_search(r#"{"query":"authenticate user token"}"#, p.0.as_path());
        assert!(out.contains("auth.rs"), "strong hit must be present:\n{out}");
        assert!(!out.contains("filler4.rs"), "trailing low-relevance chaff must be suppressed:\n{out}");
        assert!(out.contains("above relevance threshold"), "relevance cutoff note:\n{out}");

        // Explicit max_results > 10 disables cliff cutoff for callers requesting breadth
        let out_breadth = code_search(r#"{"query":"authenticate user token","max_results":15}"#, p.0.as_path());
        assert!(out_breadth.contains("filler4.rs"), "explicit breadth query includes tail:\n{out_breadth}");
    }

    #[test]
    fn generated_manifests_are_excluded_from_ranking() {
        let p = Proj::new("manifests");
        // A lockfile with a unique token, and a real source file with the same
        // token. The lockfile must NOT surface (it's excluded), the source must.
        p.write("Cargo.lock", "name = \"needle\"\nversion = \"1.0.0\"\n");
        p.write("src/lib.rs", "fn needle() {}\n");
        let out = code_search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(
            !out.contains("Cargo.lock"),
            "lockfile excluded from ranked corpus:\n{out}"
        );
        assert!(
            out.contains("src/lib.rs"),
            "real source still found:\n{out}"
        );
    }

    #[test]
    fn generated_manifest_detection_covers_common_lockfiles() {
        for name in [
            "Cargo.lock",
            "package-lock.json",
            "yarn.lock",
            "pnpm-lock.yaml",
            "composer.lock",
            "poetry.lock",
            "Pipfile.lock",
            "Gemfile.lock",
            "go.sum",
            "mix.lock",
            "packages.lock.json",
        ] {
            assert!(
                super::is_generated_manifest(name),
                "{name} should be detected"
            );
        }
        for name in ["Cargo.toml", "package.json", "go.mod", "src/lib.rs"] {
            assert!(
                !super::is_generated_manifest(name),
                "{name} should NOT be detected"
            );
        }
    }

    #[test]
    fn token_aware_chunker_respects_budget_and_line_numbers() {
        let count = |_s: &str| 10usize;
        // 5 lines × 10 tokens, budget 30 → chunks [1..3] and [4..5].
        let chunks = super::chunk_file("f.rs", "aaaa\nbbbb\ncccc\ndddd\neeee\n", 30, &count);
        assert_eq!(chunks.len(), 2);
        assert_eq!((chunks[0].start, chunks[0].end), (1, 3));
        assert_eq!((chunks[1].start, chunks[1].end), (4, 5));
        // A line bigger than the budget becomes its own chunk (and never hangs).
        let big = |_s: &str| 50usize;
        let one = super::chunk_file("f.rs", "aaaaaaaaaa\nbbbbbbbbbb\n", 30, &big);
        assert_eq!(one.len(), 2);
        assert_eq!((one[0].start, one[0].end), (1, 1));
        assert_eq!((one[1].start, one[1].end), (2, 2));
    }

    #[test]
    fn ast_chunker_keeps_doc_comments_with_their_symbol() {
        // 5 tokens/line, budget 20. Blind windowing cuts after line 4 and orphans
        // beta's doc comment into alpha's chunk; the AST chunker cuts at beta's
        // doc block instead, so docs travel with the item they describe.
        let src = "fn alpha() {\n    let a = 1;\n}\n/// Docs for beta.\nfn beta() {\n    let b = 2;\n}\n";
        let count = |_s: &str| 5usize;
        let chunks = super::chunk_file_aware("f.rs", src, 20, &count);
        assert_eq!(chunks.len(), 2, "one chunk per symbol region");
        assert_eq!((chunks[0].start, chunks[0].end), (1, 3));
        assert_eq!((chunks[1].start, chunks[1].end), (4, 7));
        assert!(
            chunks[1].text.contains("Docs for beta") && chunks[1].text.contains("fn beta"),
            "doc comment travels with its fn"
        );
        assert!(
            !chunks[0].text.contains("Docs for beta"),
            "doc comment not orphaned into the previous chunk"
        );
        let blind = super::chunk_file("f.rs", src, 20, &count);
        assert_eq!(blind[0].end, 4, "blind cut lands between doc and fn");
    }

    #[test]
    fn ast_chunker_falls_back_without_symbols_or_support() {
        let count = |_s: &str| 10usize;
        let text = "aaaa\nbbbb\ncccc\ndddd\neeee\n";
        // Unsupported extension (markdown): blind windowing, unchanged.
        let blind = super::chunk_file("README.md", text, 30, &count);
        let aware = super::chunk_file_aware("README.md", text, 30, &count);
        assert_eq!(aware.len(), blind.len());
        assert_eq!((aware[0].start, aware[0].end), (blind[0].start, blind[0].end));
        // Supported extension but no extractable symbols (a code fragment):
        // same fallback.
        let blind = super::chunk_file("frag.rs", text, 30, &count);
        let aware = super::chunk_file_aware("frag.rs", text, 30, &count);
        assert_eq!(aware.len(), blind.len());
        assert_eq!((aware[0].start, aware[0].end), (blind[0].start, blind[0].end));
    }

    #[test]
    fn ast_chunker_subdivides_oversized_symbols() {
        // One 6-line function at 10 tokens/line with a 30 budget: the whole
        // symbol is one region, so it must subdivide along lines (absolute
        // numbers preserved) instead of hanging or blowing the budget.
        let src = "fn giant() {\n    a;\n    b;\n    c;\n    d;\n}\n";
        let count = |_s: &str| 10usize;
        let chunks = super::chunk_file_aware("f.rs", src, 30, &count);
        assert_eq!(chunks.len(), 2);
        assert_eq!((chunks[0].start, chunks[0].end), (1, 3));
        assert_eq!((chunks[1].start, chunks[1].end), (4, 6));
    }

    #[test]
    fn file_cap_merges_chunks_without_dropping_the_tail() {
        let chunks: Vec<super::Chunk> = (1..=8)
            .map(|i| super::Chunk {
                path: "f.rs".to_string(),
                start: i,
                end: i,
                text: format!("line{i}"),
            })
            .collect();
        let capped = super::apply_file_cap(chunks, 3);
        assert_eq!(capped.len(), 3);
        assert_eq!((capped[0].start, capped[0].end), (1, 3));
        assert_eq!((capped[1].start, capped[1].end), (4, 6));
        assert_eq!((capped[2].start, capped[2].end), (7, 8));
        assert_eq!(capped[2].text, "line7\nline8", "tail kept verbatim");
    }

    #[test]
    fn test_and_fixture_routing_is_detected() {
        for rel in [
            "src/foo.test.ts",
            "src/foo.spec.tsx",
            "foo_test.go",
            "test_foo.py",
            "test.py",
            "tests/a.rs",
            "src/__mocks__/x.ts",
            "fixtures/data.json",
            "src/test/runner.ts",
            "__snapshots__/x.snap",
        ] {
            assert!(super::is_test_noise(rel), "{rel} should route to BM25-only");
        }
        for rel in [
            "src/lib.rs",
            "run-tests.mjs",
            "src/testing/assert.ts",
            "latest.ts",
            "contest.rs",
            "src/mocks.ts",
        ] {
            assert!(!super::is_test_noise(rel), "{rel} should stay embeddable");
        }
    }

    #[test]
    fn test_files_stay_searchable_but_never_embedded() {
        let p = Proj::new("routing");
        p.write(
            "src/foo.test.ts",
            "export function testAlpha() {\n    return 1;\n}\n",
        );
        let indexed = super::build_corpus(
            p.0.as_path(),
            &["src/foo.test.ts".to_string()],
            false,
            &CancellationToken::new(),
        );
        assert!(!indexed.is_empty(), "still in the BM25 corpus");
        assert!(indexed.iter().all(|ic| !ic.embeddable), "never embedded");
    }

    #[test]
    fn rust_inline_test_tail_is_bm25_only() {
        let p = Proj::new("tail");
        let mut body = String::new();
        for i in 0..150 {
            body.push_str(&format!("fn filler_{i}() {{\n    let a = 1;\n}}\n"));
        }
        let marker = body.lines().count() + 1;
        body.push_str("#[cfg(test)]\nmod tests {\n");
        for i in 0..120 {
            body.push_str(&format!("fn test_{i}() {{\n    assert_eq!(1, 1);\n}}\n"));
        }
        body.push_str("}\n");
        assert_eq!(
            super::rust_test_tail_line(&body),
            Some(marker),
            "test region start found"
        );
        p.write("lib.rs", &body);
        let indexed = super::build_corpus(
            p.0.as_path(),
            &["lib.rs".to_string()],
            false,
            &CancellationToken::new(),
        );
        assert!(
            indexed.iter().any(|ic| ic.embeddable),
            "head chunks stay embeddable"
        );
        let tail: Vec<usize> = indexed
            .iter()
            .filter(|ic| !ic.embeddable)
            .map(|ic| ic.chunk.start)
            .collect();
        assert!(!tail.is_empty(), "test tail routed to BM25-only");
        assert!(
            tail.iter().all(|&s| s >= marker),
            "only tail chunks demoted: {tail:?}"
        );
        let head: Vec<usize> = indexed
            .iter()
            .filter(|ic| ic.embeddable)
            .map(|ic| ic.chunk.start)
            .collect();
        assert!(
            head.iter().all(|&s| s < marker),
            "head stays embeddable: {head:?}"
        );
    }

    #[test]
    fn vendored_and_generated_dirs_are_excluded_from_ranking() {
        assert!(super::is_ranked_noise("public/pyodide/pyodide.mjs"));
        assert!(super::is_ranked_noise(
            "src-tauri/gen/schemas/desktop-schema.json"
        ));
        assert!(super::is_ranked_noise("package-lock.json"), "manifest too");
        assert!(super::is_ranked_noise(
            "src-tauri/.fastembed_cache/models--x/tokenizer.json"
        ));
        assert!(super::is_ranked_noise(".fastembed_cache/foo"));
        assert!(super::is_ranked_noise("pre_validation_runs/qwen/transcript.txt"));
        assert!(super::is_ranked_noise("logs/app.log"));
        assert!(super::is_ranked_noise("debug.log"));
        assert!(super::is_ranked_noise("session.transcript.txt"));
        assert!(!super::is_ranked_noise("src/lib.rs"));
        assert!(!super::is_ranked_noise("package.json"));
        assert!(!super::is_ranked_noise("README.md"));

        assert!(super::is_doc_file("README.md"));
        assert!(super::is_doc_file("docs/architecture.markdown"));
        assert!(super::is_doc_file("notes.txt"));
        assert!(!super::is_doc_file("src/main.rs"));
    }

    #[test]
    fn path_filter_restricts_search_scope() {
        let p = Proj::new("path_filter");
        p.write("src/a.rs", "fn compute_total() {}\n");
        p.write("other/b.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","path":"src"}"#, p.0.as_path());
        assert!(out.contains("src/a.rs"), "scoped path must match:\n{out}");
        assert!(!out.contains("other/b.rs"), "unscoped path must be excluded:\n{out}");
    }

    #[test]
    fn path_filter_normalizes_windows_slashes() {
        let p = Proj::new("path_win");
        p.write("src/sub/a.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","path":"src\\sub\\"}"#, p.0.as_path());
        assert!(out.contains("src/sub/a.rs"), "windows slash path must match:\n{out}");
    }

    #[test]
    fn path_filter_honest_miss_when_no_files() {
        let p = Proj::new("path_miss");
        p.write("src/a.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","path":"nonexistent"}"#, p.0.as_path());
        assert!(out.contains("matched no files"), "honest path miss:\n{out}");
    }

    #[test]
    fn path_filter_normalizes_absolute_project_paths() {
        let p = Proj::new("path_abs");
        p.write("src/sub/a.rs", "fn compute_total() {}\n");
        let abs_path = p.0.join("src").join("sub").to_string_lossy().to_string();
        let payload = serde_json::json!({
            "query": "compute total",
            "path": abs_path,
        })
        .to_string();
        let out = code_search(&payload, p.0.as_path());
        assert!(out.contains("src/sub/a.rs"), "absolute path should resolve seamlessly:\n{out}");
    }

    #[test]
    fn path_and_glob_filter_blames_path_when_path_misses() {
        let p = Proj::new("blame_path");
        p.write("src/a.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","path":"nonexistent","glob":"*.rs"}"#, p.0.as_path());
        assert!(out.starts_with("Path 'nonexistent' matched no files"), "should blame path, not glob:\n{out}");
    }

    #[test]
    fn path_and_glob_filter_blames_glob_when_path_matches_but_glob_misses() {
        let p = Proj::new("blame_glob");
        p.write("src/a.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","path":"src","glob":"*.py"}"#, p.0.as_path());
        assert!(out.starts_with("Glob '*.py' matched no files"), "should blame glob when path had matches:\n{out}");
    }

    #[test]
    fn glob_filter_restricts_search_scope() {
        let p = Proj::new("glob_filter");
        p.write("src/sub/a.rs", "fn compute_total() {}\n");
        p.write("src/sub/a.ts", "function compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","glob":"*.rs"}"#, p.0.as_path());
        assert!(out.contains("src/sub/a.rs"), "glob match on subfolder file:\n{out}");
        assert!(!out.contains("a.ts"), "extension mismatch excluded:\n{out}");
    }

    #[test]
    fn glob_filter_honest_miss_when_no_files() {
        let p = Proj::new("glob_miss");
        p.write("src/a.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","glob":"*.py"}"#, p.0.as_path());
        assert!(out.contains("matched no files"), "honest glob miss:\n{out}");
    }

    #[test]
    fn invalid_glob_returns_error() {
        let p = Proj::new("bad_glob");
        p.write("src/a.rs", "fn compute_total() {}\n");
        let out = code_search(r#"{"query":"compute total","glob":"[invalid"}"#, p.0.as_path());
        assert!(out.starts_with("error: invalid glob"), "error on malformed glob:\n{out}");
    }

    // ── end-to-end (real model) — opt-in: needs bge-small downloaded ──

    #[test]
    #[ignore = "needs bge-small-en-v1.5 downloaded (slow, network on first run)"]
    fn hybrid_finds_semantic_match_without_shared_tokens() {
        let p = Proj::new("hybrid");
        p.write(
            "src/net.rs",
            "fn open_socket() {}\nfn read_packet() {}\nfn close_socket() {}\n",
        );
        p.write("src/scratch.rs", "fn update_scratchpad() {}\n");
        // No token overlap with the net identifiers, so BM25 alone misses; the
        // vector arm must surface src/net.rs.
        let out = search_with(
            r#"{"query":"how do we receive data over the network"}"#,
            p.0.as_path(),
            true,
            None,
            &CancellationToken::new(),
        );
        assert!(out.contains("src/net.rs"), "semantic hit:\n{out}");
        assert!(
            !out.contains("BM25-only"),
            "vector arm should be active:\n{out}"
        );
    }

    #[test]
    #[ignore = "needs bge-small-en-v1.5 downloaded (slow, network on first run)"]
    fn hybrid_nonsense_query_is_an_honest_miss() {
        let p = Proj::new("hybrid_miss");
        p.write("src/a.rs", "fn alpha() {}\nfn beta() {}\n");
        let out = search_with(
            r#"{"query":"zzz_qxwv_irrelevant_nonsense"}"#,
            p.0.as_path(),
            true,
            None,
            &CancellationToken::new(),
        );
        assert!(
            out.starts_with("No relevant matches"),
            "nonsense query must not manufacture a hit:\n{out}"
        );
    }

    #[test]
    fn persisted_index_roundtrips_and_model_key_guards() {
        let vectors = vec![(12345u64, vec![0.1f32, -0.2f32])];
        let persisted = super::PersistedIndex {
            model: super::MODEL_KEY.to_string(),
            vectors: vectors.clone(),
        };
        let bytes = serde_json::to_vec(&persisted).expect("serializes");
        let back: super::PersistedIndex = serde_json::from_slice(&bytes).expect("deserializes");
        assert_eq!(back.model, super::MODEL_KEY);
        assert_eq!(back.vectors, vectors);

        // A wrong model key must be rejected (stale-dimension guard).
        let wrong = super::PersistedIndex {
            model: "some-other-model".to_string(),
            vectors: vectors.clone(),
        };
        let dir = std::env::temp_dir().join(format!("outpost_idx_guard_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("guard.json");
        std::fs::write(&path, serde_json::to_vec(&wrong).unwrap()).unwrap();
        assert!(super::load_index(&path).is_none(), "wrong model rejected");

        // The pre-two-stage key must be rejected too:
        let pre_two_stage = super::PersistedIndex {
            model: "bge-small-en-v1.5/ast-chunk-1".to_string(),
            vectors,
        };
        std::fs::write(&path, serde_json::to_vec(&pre_two_stage).unwrap()).unwrap();
        assert!(super::load_index(&path).is_none(), "pre-two-stage index rejected");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[ignore = "needs bge-small-en-v1.5 downloaded (slow, network on first run)"]
    fn persisted_index_is_written_and_reused() {
        let p = Proj::new("persist");
        // Cache lives OUTSIDE the project root (in production it's the app-data
        // dir) — a cache inside the walked tree would index its own JSON and
        // re-embed on every search, so the test mirrors the real layout.
        let cache = std::env::temp_dir().join(format!(
            "outpost_code_search_persist_cache_{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&cache);
        p.write("src/net.rs", "fn open_socket() {}\nfn read_packet() {}\n");
        let q = r#"{"query":"how do we receive data over the network"}"#;
        let out1 = search_with(
            q,
            p.0.as_path(),
            true,
            Some(cache.as_path()),
            &CancellationToken::new(),
        );
        assert!(out1.contains("src/net.rs"), "first search hit:\n{out1}");

        let dir = cache.join("search_index");
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .expect("index dir created")
            .map(|e| e.unwrap().path())
            .collect();
        let vec_file = entries
            .iter()
            .find(|p| {
                p.extension().map(|e| e == "json").unwrap_or(false)
                    && !p.to_string_lossy().ends_with(".corpus.json")
            })
            .expect("vector index exists");
        let corpus_file = entries
            .iter()
            .find(|p| p.to_string_lossy().ends_with(".corpus.json"))
            .expect("corpus index exists");

        assert!(
            super::load_index(vec_file).is_some(),
            "persisted index loads with matching model key"
        );
        assert!(
            super::load_corpus(corpus_file).is_some(),
            "persisted corpus loads cleanly"
        );

        // A second search resolves from the hydrated cache, and the index file is
        // NOT rewritten (nothing changed → no re-embed, no re-persist).
        let mtime_before = std::fs::metadata(vec_file).unwrap().modified().unwrap();
        let out2 = search_with(
            q,
            p.0.as_path(),
            true,
            Some(cache.as_path()),
            &CancellationToken::new(),
        );
        assert!(out2.contains("src/net.rs"), "second search hit:\n{out2}");
        let mtime_after = std::fs::metadata(vec_file).unwrap().modified().unwrap();
        assert_eq!(
            mtime_before, mtime_after,
            "unchanged index is not rewritten"
        );

        let _ = std::fs::remove_dir_all(&cache);
    }

    #[test]
    fn content_cache_is_bounded_and_retains_entries() {
        let mut cache = super::ContentVectorCache::new();
        cache.insert(1, vec![0.1]);
        cache.insert(2, vec![0.2]);
        assert_eq!(cache.get(1), Some(&vec![0.1]));
        assert_eq!(cache.get(2), Some(&vec![0.2]));
        assert_eq!(cache.get(3), None);
    }

    #[test]
    fn content_cache_scoped_searches_share_vectors() {
        let mut cache = super::ContentVectorCache::new();
        cache.insert(100, vec![0.5]);
        cache.insert(200, vec![0.6]);
        // Scoped search on one file leaves vectors of other files intact:
        assert_eq!(cache.get(100), Some(&vec![0.5]));
        assert_eq!(cache.get(200), Some(&vec![0.6]));
    }

    #[test]
    fn two_stage_candidate_selection_honors_budget() {
        let p = Proj::new("two_stage");
        for i in 0..250 {
            p.write(&format!("f{i}.rs"), &format!("fn needle_{i}() {{}}\n"));
        }
        let out = code_search(r#"{"query":"needle"}"#, p.0.as_path());
        assert!(out.contains(".rs"), "finds needle hits:\n{out}");
    }

    #[test]
    #[ignore = "needs bge-small-en-v1.5 downloaded (slow, network on first run)"]
    fn two_stage_ephemeral_search_with_noise_files() {
        let p = Proj::new("two_stage_noise");
        p.write("src/target.rs", "fn calculate_quantum_variance() {}\n");
        for i in 0..30 {
            p.write(
                &format!("noise/dump_{i}.txt"),
                &format!("random telemetry log entry {i} without relevant content\n"),
            );
        }
        let out = search_with(
            r#"{"query":"quantum variance"}"#,
            p.0.as_path(),
            true,
            None,
            &CancellationToken::new(),
        );
        assert!(out.contains("src/target.rs"), "target found:\n{out}");
    }

    #[test]
    fn corpus_cache_reuses_clean_files_and_updates_dirty_files() {
        let p = Proj::new("corpus_incremental");
        p.write("src/clean.rs", "fn keep_me_unchanged() {}\n");
        p.write("src/dirty.rs", "fn initial_alpha_state() {}\n");

        let out1 = code_search(r#"{"query":"initial alpha"}"#, p.0.as_path());
        assert!(out1.contains("src/dirty.rs"), "first search found dirty.rs");

        // Small sleep so filesystem mtime changes measurably on all platforms
        std::thread::sleep(std::time::Duration::from_millis(50));
        p.write("src/dirty.rs", "fn updated_beta_state() {}\n");

        let out2 = code_search(r#"{"query":"updated beta"}"#, p.0.as_path());
        assert!(out2.contains("src/dirty.rs"), "second search caught modified content");

        let out3 = code_search(r#"{"query":"keep me unchanged"}"#, p.0.as_path());
        assert!(out3.contains("src/clean.rs"), "unchanged file remains indexed");
    }

    #[test]
    fn warm_cache_prepopulates_corpus() {
        let p = Proj::new("warm_cache");
        p.write("src/lib.rs", "pub fn perform_handshake() {}\n");
        super::warm_cache(p.0.as_path(), None);

        let out = code_search(r#"{"query":"perform handshake"}"#, p.0.as_path());
        assert!(out.contains("src/lib.rs"), "search resolves from warmed corpus");
    }

    #[test]
    #[ignore = "needs fastembed model resident"]
    fn index_repository_full_progress_and_subsequent_up_to_date() {
        let p = Proj::new("index_full");
        p.write("src/core.rs", "pub fn engine_start() {}\n");
        p.write("src/util.rs", "pub fn parse_input() {}\n");

        let mut events = Vec::new();
        let cancel = CancellationToken::new();
        let res = super::index_repository_full(p.0.as_path(), None, &cancel, |prog| {
            events.push(prog);
        });
        assert!(res.is_ok());
        assert!(!events.is_empty());
        let last = events.last().unwrap();
        assert_eq!(last.status, "done");

        // Second run should be up_to_date
        let mut second_events = Vec::new();
        let res2 = super::index_repository_full(p.0.as_path(), None, &cancel, |prog| {
            second_events.push(prog);
        });
        assert!(res2.is_ok());
        assert_eq!(second_events.len(), 1);
        assert_eq!(second_events[0].status, "up_to_date");
    }

    #[test]
    fn index_repository_full_cancels_cleanly() {
        let p = Proj::new("index_cancel");
        p.write("src/lib.rs", "pub fn immediate_cancel() {}\n");

        let cancel = CancellationToken::new();
        cancel.cancel(); // pre-cancelled
        let mut events = Vec::new();
        let res = super::index_repository_full(p.0.as_path(), None, &cancel, |prog| {
            events.push(prog);
        });
        assert!(res.is_ok());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].status, "cancelled");
    }

    #[test]
    fn doc_chunks_capped_and_candidate_quota_enforced() {
        let p = Proj::new("doc_quota");
        // Create a massive markdown file
        let mut big_doc = String::new();
        for i in 0..2000 {
            big_doc.push_str(&format!("Line {i}: documentation prose describing architecture concept\n"));
        }
        p.write("docs/ARCHITECTURE.md", &big_doc);

        let indexed = super::build_corpus(
            p.0.as_path(),
            &["docs/ARCHITECTURE.md".to_string()],
            false,
            &CancellationToken::new(),
        );
        // Ensure chunks for the single doc file are capped to MAX_DOC_FILE_CHUNKS (16)
        assert!(
            indexed.len() <= super::MAX_DOC_FILE_CHUNKS,
            "doc chunks capped at {}: got {}",
            super::MAX_DOC_FILE_CHUNKS,
            indexed.len()
        );
    }

    #[test]
    fn warmth_aware_candidate_selection_includes_all_warm_chunks() {
        let p = Proj::new("warmth_all");
        for i in 0..250 {
            p.write(&format!("src/file_{i}.rs"), &format!("fn unique_function_{i}() {{}}\n"));
        }
        let cancel = CancellationToken::new();
        let indexed = super::build_corpus(
            p.0.as_path(),
            &(0..250).map(|i| format!("src/file_{i}.rs")).collect::<Vec<_>>(),
            false,
            &cancel,
        );
        assert_eq!(indexed.len(), 250);

        // Pre-warm the cache for all 250 chunks
        {
            let mut cache = super::content_cache().lock().unwrap();
            for ic in &indexed {
                let h = super::fnv1a64(ic.chunk.text.as_bytes());
                cache.insert(h, vec![0.1f32; 384]);
            }
        }

        // Even though corpus has 250 chunks (> CANDIDATE_BUDGET=200),
        // because all chunks are warm, cold_set is 0 <= 200, so ALL 250 chunks are candidates!
        let (warm, cold): (Vec<usize>, std::collections::HashSet<usize>) = {
            let cache = super::content_cache().lock().unwrap();
            let mut w = Vec::new();
            let mut c = std::collections::HashSet::new();
            for (idx, ic) in indexed.iter().enumerate() {
                let h = super::fnv1a64(ic.chunk.text.as_bytes());
                if cache.contains(h) {
                    w.push(idx);
                } else {
                    c.insert(idx);
                }
            }
            (w, c)
        };
        assert_eq!(warm.len(), 250);
        assert_eq!(cold.len(), 0);
    }

    #[test]
    fn incremental_edit_preserves_full_coverage() {
        let p = Proj::new("warmth_edit");
        for i in 0..220 {
            p.write(&format!("src/file_{i}.rs"), &format!("fn existing_func_{i}() {{}}\n"));
        }
        let cancel = CancellationToken::new();
        let indexed = super::build_corpus(
            p.0.as_path(),
            &(0..220).map(|i| format!("src/file_{i}.rs")).collect::<Vec<_>>(),
            false,
            &cancel,
        );
        assert_eq!(indexed.len(), 220);

        // Pre-warm the cache for the first 218 chunks (simulate an indexed repo where 2 files are edited)
        {
            let mut cache = super::content_cache().lock().unwrap();
            for ic in &indexed[..218] {
                let h = super::fnv1a64(ic.chunk.text.as_bytes());
                cache.insert(h, vec![0.1f32; 384]);
            }
        }

        let (warm, cold): (Vec<usize>, std::collections::HashSet<usize>) = {
            let cache = super::content_cache().lock().unwrap();
            let mut w = Vec::new();
            let mut c = std::collections::HashSet::new();
            for (idx, ic) in indexed.iter().enumerate() {
                let h = super::fnv1a64(ic.chunk.text.as_bytes());
                if cache.contains(h) {
                    w.push(idx);
                } else {
                    c.insert(idx);
                }
            }
            (w, c)
        };
        assert_eq!(warm.len(), 218);
        assert_eq!(cold.len(), 2);
        // Because cold_set.len() (2) <= CANDIDATE_BUDGET (200), total candidates = 218 + 2 = 220! Zero cliff!
        assert!(cold.len() <= super::CANDIDATE_BUDGET);
    }

    #[test]
    fn embedding_model_status_reports_correctly() {
        // The global FASTEMBED_CACHE_DIR (set at app startup in lib.rs) points to the
        // real app-data models directory, which may already contain the cached model.
        // Temporarily reset it so embedding_model_status derives the cache dir from the
        // provided app_data_dir (the throwaway Proj), letting the test verify its status logic
        // against an isolated cache location, then restore it on drop.
        struct EnvVarGuard {
            key: &'static str,
            prev: Option<std::ffi::OsString>,
        }
        impl Drop for EnvVarGuard {
            fn drop(&mut self) {
                match &self.prev {
                    Some(v) => std::env::set_var(self.key, v),
                    None => std::env::remove_var(self.key),
                }
            }
        }
        let _guard = EnvVarGuard {
            key: "FASTEMBED_CACHE_DIR",
            prev: std::env::var_os("FASTEMBED_CACHE_DIR"),
        };
        std::env::remove_var("FASTEMBED_CACHE_DIR");
        let p = Proj::new("embed_status");
        let status = super::embedding_model_status(Some(p.0.as_path()));
        assert_eq!(status.model_name, "bge-small-en-v1.5");
        assert_eq!(status.status, "not_downloaded");
        assert_eq!(status.size_bytes, 0);

        // Simulate a downloaded model structure
        let model_dir = p
            .0
            .join("models")
            .join("models--Xenova--bge-small-en-v1.5")
            .join("snapshots")
            .join("rev1");
        fs::create_dir_all(&model_dir).unwrap();
        let onnx_path = model_dir.join("model.onnx");
        let dummy_data = vec![0u8; 11 * 1024 * 1024];
        fs::write(&onnx_path, &dummy_data).unwrap();

        let status2 = super::embedding_model_status(Some(p.0.as_path()));
        assert_eq!(status2.status, "ready");
        assert!(status2.size_bytes >= 11 * 1024 * 1024);
        assert!(status2.size_formatted.contains("MB"));
    }

    #[test]
    fn format_size_handles_units() {
        assert_eq!(super::format_size(0), "0 B");
        assert_eq!(super::format_size(1024 * 1024 * 50), "50.0 MB");
        assert_eq!(super::format_size(1024 * 1024 * 1024 * 2), "2.00 GB");
    }

    #[test]
    fn hydrate_clears_vectors_when_switching_roots() {
        let mut cache = super::ContentVectorCache::new();
        cache.insert(42, vec![1.0, 2.0, 3.0]);
        cache.loaded_root = Some("repo_a".to_string());

        let temp = Proj::new("hydrate_switch");
        let cache_dir = temp.0.join("cache");
        let repo_b = temp.0.join("repo_b");
        fs::create_dir_all(&repo_b).unwrap();

        // Hydrating for repo_b should clear repo_a's vectors
        super::hydrate_locked(&mut cache, &repo_b, Some(&cache_dir));
        assert!(cache.get(42).is_none(), "vectors from previous root must be cleared");
        assert_eq!(cache.entries.len(), 0);
        assert_eq!(cache.order.len(), 0);
        assert!(cache.loaded_root.is_some());
    }

    #[test]
    fn lru_order_is_promoted_on_get_and_preserved_on_disk() {
        let mut cache = super::ContentVectorCache::new();
        cache.insert(1, vec![1.0]);
        cache.insert(2, vec![2.0]);
        cache.insert(3, vec![3.0]);
        assert_eq!(cache.order.iter().copied().collect::<Vec<_>>(), vec![1, 2, 3]);

        // Accessing 1 moves it to the back (most recently used)
        let _ = cache.get(1);
        assert_eq!(cache.order.iter().copied().collect::<Vec<_>>(), vec![2, 3, 1]);

        // Persist to disk and hydrate into a fresh cache
        let temp = Proj::new("lru_persist");
        let cache_dir = temp.0.join("cache");
        let repo = temp.0.join("repo");
        fs::create_dir_all(&repo).unwrap();

        super::persist_index(&cache, &repo, Some(&cache_dir));

        let mut fresh = super::ContentVectorCache::new();
        super::hydrate_locked(&mut fresh, &repo, Some(&cache_dir));

        // Hydrated cache must preserve the exact same LRU order: [2, 3, 1]
        assert_eq!(fresh.order.iter().copied().collect::<Vec<_>>(), vec![2, 3, 1]);
        assert_eq!(fresh.get(2), Some(&vec![2.0]));
        assert_eq!(fresh.order.iter().copied().collect::<Vec<_>>(), vec![3, 1, 2]);
    }
}
