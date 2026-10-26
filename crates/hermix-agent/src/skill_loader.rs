//! Skill loader — auto-matches user messages to skills and injects instructions.
//!
//!
//!
//! The existing [`SkillOrchestrator`] handles slash commands (`/skill-name`)
//! and session-wide preloading. This module adds **automatic skill matching**:
//!
//! 1. **Keyword matching**: When a user message contains keywords that match
//!    a skill's name, description, or tags, the skill's instructions are
//!    injected into the system prompt for that turn.
//!
//! 2. **Capability gap fallback** (`find-skills`): When the LLM encounters a
//!    task it can't complete with the available tools, it can call
//!    `find-skills` to discover skills that might help. This is the
//!    "能力缺口兜底" (capability gap backstop) from the 铁律 agent_skills.
//!
//! ## Integration
//!
//! The orchestration core calls [`SkillLoader::match_message`] at the start
//! of each turn with the user's message text. If a skill matches, its
//! instructions are returned and injected into the system prompt alongside
//! the existing `<available_skills>` block.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::skill_orchestrator::{SkillCommandInfo, SkillOrchestrator};

// ---------------------------------------------------------------------------
// SkillMatch
// ---------------------------------------------------------------------------

/// A skill that matched a user message.
#[derive(Debug, Clone)]
pub struct SkillMatch {
    /// The slash command key (e.g. `/commit-message`).
    pub command_key: String,
    /// The skill name.
    pub name: String,
    /// The skill description.
    pub description: String,
    /// The relevance score (0.0 to 1.0).
    pub score: f32,
    /// The full skill body (instructions to inject).
    pub body: String,
    /// Which field matched: "name", "description", or "tags".
    pub matched_on: String,
    /// `allowed-tools` declared by the skill (None / empty = unrestricted).
    pub allowed_tools: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// SkillLoader
// ---------------------------------------------------------------------------

/// 进程级扫描缓存条目（键是技能目录）。
struct CachedLoader {
    /// 「目录 mtime + 条目数」混合指纹：增删技能目录会变，改文件内容不会。
    stamp: u64,
    loaded_at: std::time::Instant,
    loader: Arc<SkillLoader>,
}

/// 取一个已 `scan()` 过的 [`SkillLoader`]，按目录缓存。
///
/// `with_dir + scan()` 实测 **25.6 ms/次**（30 个技能，debug 构建；见
/// `probe_scan_cost`），而 `pre_turn` 每轮都要调一次，且它是在 async 路径上
/// 同步跑（阻塞 tokio worker）。
///
/// 缓存键 = 目录 mtime + 条目数 + 60s TTL，三种失效路径分别是：
/// - **增删技能**（目录 mtime 变）→ 立即重扫；
/// - **改 frontmatter**（描述/标签/允许工具）→ 最多 60s 后生效；
/// - **改正文** → **立即生效**：`load_body` 每次匹配后都重新读盘，正文从不过缓存。
///
/// 只有第二条是这套缓存的代价。用户改技能绝大多数是改正文，而每轮 25ms ×
/// 每次都付，换这条 60s 的元数据延迟是划算的。
pub fn cached_loader(dir: &Path) -> Arc<SkillLoader> {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    /// 元数据刷新的上限延迟。
    const TTL: Duration = Duration::from_secs(60);

    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedLoader>>> = OnceLock::new();

    fn stamp(dir: &Path) -> u64 {
        use std::time::UNIX_EPOCH;
        let mtime = std::fs::metadata(dir)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let entries = std::fs::read_dir(dir).map(|d| d.count() as u64).unwrap_or(0);
        // 两个小数字混成一位指纹：只要任一变就重扫。
        mtime ^ entries.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    }

    let stamp = stamp(dir);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Ok(guard) = cache.lock() {
        if let Some(entry) = guard.get(dir) {
            if entry.stamp == stamp && entry.loaded_at.elapsed() < TTL {
                return entry.loader.clone();
            }
        }
    }

    let mut loader = SkillLoader::with_dir(dir);
    loader.scan();
    let loader = Arc::new(loader);
    if let Ok(mut guard) = cache.lock() {
        guard.insert(
            dir.to_path_buf(),
            CachedLoader {
                stamp,
                loaded_at: Instant::now(),
                loader: loader.clone(),
            },
        );
    }
    loader
}

/// Auto-matches user messages to skills and produces injection text.
/// Wraps a [`SkillOrchestrator`] and adds keyword-based matching on top of
/// the slash-command resolution. The orchestrator handles discovery (scanning
/// `~/.hermix/skills/` for SKILL.md files); the loader adds the matching
/// layer.
pub struct SkillLoader {
    orchestrator: SkillOrchestrator,
    /// Minimum score threshold for a match (default 0.3).
    min_score: f32,
    /// Maximum number of auto-matched skills to inject per turn (default 3).
    max_matches: usize,
    /// Maximum lines of an auto-injected SKILL.md body (default 500).
    /// Over-cap bodies degrade to a section SKELETON (see `load_body`),
    /// not a truncated half-article.
    max_body_lines: usize,
    /// Vector-recall threshold for **non-CJK** queries (cosine over the
    /// description).
    ///
    /// Default 0.52. History of the number, both steps measured:
    ///
    /// - 0.18 → 0.45 (2026-10-21, synthetic corpus in
    ///   `tests/skill_routing_bench.rs`): every observed false activation came
    ///   from this layer at cosines 0.19–0.38.
    /// - 0.45 → 0.52 (same day, real library — `bench_against_real_library`,
    ///   36 skills): the noise band widens with library size —
    ///   "the printer is not responding" hit `frontend-architecture` at
    ///   **0.50** on real descriptions. The keyword / CJK / fuzzy passes
    ///   produced zero false positives on both corpora, which is why the fix
    ///   is a threshold and not a scoring rewrite.
    ///
    /// The hashing bigram embedder hands *any* English short text a broad
    /// cosine band against *any* English description; the bar must sit above
    /// whatever the current library's worst noise is, so re-measure via
    /// `bench_against_real_library` whenever the library grows substantially.
    /// What is deliberately NOT known: the score floor of genuine
    /// vector-only positives on the real library (none observed yet). If one
    /// ever misses, lower *this* number — do not touch the CJK one.
    ///
    /// CJK queries use [`Self::semantic_min_score_cjk`] instead — see there
    /// for why one number cannot serve both scripts.
    /// Only governs skills matching exclusively via vector; keyword matches
    /// keep their score. An operator raising `min_score` above its 0.3
    /// default raises this bar too.
    semantic_min_score: f32,

    /// Vector-recall threshold for **CJK** queries.
    ///
    /// Stays at 0.18 while the English bar moved to 0.45, because the two
    /// scripts do not share a noise floor. The 0.19–0.38 false positives that
    /// forced the English bar up were all English; CJK bigram hashing produces
    /// a different distribution where a real paraphrase and an unrelated
    /// sentence *are* separable — proven by the two pre-existing tests
    /// `vector_recall_matches_cjk_paraphrase_keyword_misses` (「帮我美化一下
    /// 界面配色」 → impeccable-design) and `vector_recall_does_not_fire_on_
    /// unrelated_query` (「帮我清理C盘垃圾文件」 → 不命中). Killing that
    /// capability to fix English noise would trade a working feature for a
    /// measurement we never took.
    ///
    /// If a CJK false positive ever appears, measure its cosine and raise
    /// *this* number — do not lower the English one to compensate.
    semantic_min_score_cjk: f32,
}

impl SkillLoader {
    /// Create a new loader using the default skills directory.
    pub fn new() -> Self {
        Self {
            orchestrator: SkillOrchestrator::default_dir(),
            min_score: 0.3,
            max_matches: 3,
            max_body_lines: 500,
            semantic_min_score: 0.52,
            semantic_min_score_cjk: 0.18,
        }
    }

    /// Create a new loader with a specific skills directory.
    pub fn with_dir(skills_dir: impl Into<PathBuf>) -> Self {
        Self {
            orchestrator: SkillOrchestrator::new(skills_dir),
            min_score: 0.3,
            max_matches: 3,
            max_body_lines: 500,
            semantic_min_score: 0.52,
            semantic_min_score_cjk: 0.18,
        }
    }

    /// Set the minimum score threshold.
    pub fn with_min_score(mut self, score: f32) -> Self {
        self.min_score = score;
        self
    }

    /// Set the maximum number of auto-matched skills per turn.
    pub fn with_max_matches(mut self, max: usize) -> Self {
        self.max_matches = max;
        self
    }

    /// Override the per-skill body line cap. Default is 500; we
    /// expose the setter so tests can pin a tiny cap (5 lines) to
    /// assert the skeleton degradation fires deterministically.
    pub fn with_max_body_lines(mut self, max: usize) -> Self {
        self.max_body_lines = max;
        self
    }

    /// Override the vector-recall cosine threshold (default 0.30).
    /// Exposed for tests to pin deterministic behavior.
    pub fn with_semantic_min_score(mut self, min: f32) -> Self {
        self.semantic_min_score = min;
        self
    }

    /// Scan for skills (delegates to the orchestrator).
    pub fn scan(&mut self) -> &HashMap<String, SkillCommandInfo> {
        self.orchestrator.scan_skill_commands()
    }

    /// Get the underlying orchestrator.
    pub fn orchestrator(&self) -> &SkillOrchestrator {
        &self.orchestrator
    }

    /// Match a user message to skills.
    /// Returns up to `max_matches` skills whose name, description, or tags
    /// contain keywords from the message. The score is based on the ratio
    /// of matched keywords to total message keywords. added a
    /// fuzzy subscore on top.
    /// Effective threshold scales with query length: a single-token
    /// query needs ≥ 0.3 (rich signal required), a multi-token query
    /// only needs ≥ 0.15 (because 1 hit / 6 stop-words is a real
    /// relevance signal that a fixed 0.3 threshold would suppress).
    pub fn match_message(&self, message: &str) -> Vec<SkillMatch> {
        let commands = self.orchestrator.get_commands();
        if commands.is_empty() {
            return Vec::new();
        }

        let msg_lower = message.to_lowercase();
        let msg_tokens: Vec<&str> = msg_lower
            .split_whitespace()
            .filter(|t| t.len() >= 3) // skip short tokens
            .filter(|t| !STOPWORDS.contains(t)) // skip trivial words
            .collect();

        if message.trim().is_empty() {
            return Vec::new();
        }

        // Token-aware effective threshold — but ONLY soften the
        // default `min_score`. Operator-supplied thresholds
        // (e.g. a test pinning min_score = 0.99) are honored
        // verbatim regardless of query length.
        let effective_min = if msg_tokens.len() == 1 {
            self.min_score.max(0.3)
        } else if self.min_score <= 0.3 {
            0.15
        } else {
            self.min_score
        };

        let mut scored: Vec<SkillMatch> = Vec::new();

        for (cmd_key, info) in commands {
            let name_lower = info.name.to_lowercase();
            let desc_lower = info.description.to_lowercase();
            let tags_lower: Vec<String> = info.tags.iter().map(|t| t.to_lowercase()).collect();

            // Whole-word desc-set so the cheap keyword pass doesn't
            // light up on substrings like "and" inside "safe and
            // effective". The same split is used by the fuzzy helper
            // later in the loop, so we cache it once per skill.
            let desc_words: std::collections::HashSet<&str> = desc_lower
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty())
                .collect();

            // Score: how many message tokens appear in the skill's fields.
            //: we blend the cheap keyword-overlap pass with
            // a subsequence-based fuzzy score so a query like "compres"
            // still surfaces "context-compression". The exact-token score
            // always wins over the fuzzy one — we never want fuzzy hits to
            // crowd out a clean keyword match.
            let mut matched_tokens = 0;
            let mut matched_on = String::new();

            for token in &msg_tokens {
                let in_name = name_lower.split(|c: char| !c.is_alphanumeric())
                    .filter(|w| !w.is_empty())
                    .any(|w| w == *token);
                let in_desc = desc_words.contains(*token);
                // Tags are short tags; we accept both whole-tag
                // equality (preserving hyphens, "best-practices")
                // and word-equality on the hyphen-split so a
                // bare query token like "best" can still surface
                // a "best-practices"-tagged skill.
                let in_tags = tags_lower.iter().any(|t| {
                    if t == *token {
                        return true;
                    }
                    t.split(|c: char| !c.is_alphanumeric())
                        .filter(|w| !w.is_empty())
                        .any(|w| w == *token)
                });

                if in_name || in_desc || in_tags {
                    matched_tokens += 1;
                    if in_name && matched_on.is_empty() {
                        matched_on = "name".into();
                    } else if in_desc && matched_on.is_empty() {
                        matched_on = "description".into();
                    } else if in_tags && matched_on.is_empty() {
                        matched_on = "tags".into();
                    }
                }
            }

            // --- CJK (Chinese) trigger fallback ---
            // The English-oriented tokenizer above keeps a Chinese
            // sentence (no whitespace) as one long token, so it
            // never matches the short description words.
            // `cjk_bigram_hit` adds a curated bigram-overlap check
            // against the skill's tags/name so Chinese queries fire.
            // See the helper for the false-positive bounding.
            if cjk_bigram_hit(&msg_lower, &name_lower, &tags_lower) {
                matched_tokens = matched_tokens.max(1);
                if matched_on.is_empty() {
                    matched_on = "cjk".to_string();
                }
            }

            let keyword_score = if msg_tokens.is_empty() {
                0.0
            } else {
                matched_tokens as f32 / msg_tokens.len() as f32
            };
            let fuzzy_score = description_fuzzy_score(&msg_tokens, &desc_lower);
            let raw = keyword_score.max(fuzzy_score);
            let auto = is_auto_generated(info);
            // 沉淀产物降权**在阈值判断之前**：降权后不足阈值的就淘汰，
            // 否则它仍会占着 top-N 的名额把真正的手写技能挤出去。
            let score = if auto {
                raw * AUTO_SKILL_SCORE_FACTOR
            } else {
                raw
            };

            // 第二层：沉淀产物**不走软化阈值**。
            //
            // `effective_min` 对多词查询会软化到 0.15（为了保住释义召回）。
            // 那个软化对"人手挑出来的几十个技能"是合理的，对"跑几十个任务
            // 就多几十个、质量参差"的沉淀产物则是灾难：2/10 词命中即可入池。
            // 单靠 0.75 的降权只改变排序、不改变门槛——实测证实了这点
            // （raw 0.25 → 0.1875 依然 > 0.15 而入池）。
            let threshold = if auto {
                effective_min.max(AUTO_SKILL_MIN_SCORE)
            } else {
                effective_min
            };
            if score < threshold {
                continue;
            }

            // Load the skill body (over-cap → skeleton, never a half-article)
            let body = match self.load_body(&info.skill_md_path) {
                Some(b) => b,
                None => continue,
            };

            scored.push(SkillMatch {
                command_key: cmd_key.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                score,
                body,
                matched_on,
                allowed_tools: info.allowed_tools.clone(),
            });
        }

        // --- Vector recall (semantic layer, additive) ---
        // Keyword/CJK/fuzzy passes are literal: a same-meaning paraphrase
        // ("界面美化配色" vs a skill written as "视觉设计规范") misses them
        // all. This pass embeds (name + description + tags) and the query
        // with the always-on hashing embedder (pure CPU, no network, no
        // model download; the fastembed upgrade is feature-gated and slots
        // in behind the same cosine contract). A skill matching ONLY via
        // vector must clear `semantic_min_score`; keyword matches keep
        // their score (exact-token signal always wins on ties).
        let embedder =
            hermix_intelligence::embedding::HashingEmbeddingProvider::with_ngram(512, 2);
        // Embed the STOPWORD-FILTERED query (English glue bigrams like
        // "at"/"er" from "that"/"are" otherwise dominate short-text
        // cosine and false-positive generic descriptions). CJK runs are
        // alphanumeric → survive the token filter intact; when filtering
        // empties the query (pure-CJK, no whitespace) fall back to the
        // raw message.
        let query_for_vec = if msg_tokens.is_empty() {
            message.to_string()
        } else {
            msg_tokens.join(" ")
        };
        let query_vec = embedder.embed_sync(&query_for_vec);
        // Operator strictness link: raising `min_score` ABOVE the 0.3
        // default raises the semantic bar with it (a pinned 0.99 test /
        // strict operator must not get vector matches the keyword layer
        // would reject). The 0.3 default itself does NOT leak into the
        // semantic layer — CJK paraphrase cosines sit around 0.15-0.25.
        //
        // `max`, not a conditional swap: the old `if min_score > 0.3 {
        // min_score } else { semantic_min_score }` **lowered** the vector bar
        // for any operator in the 0.30–0.45 window (min_score=0.35 →
        // semantic threshold 0.35, below the 0.45 default). The 2026-10-20
        // threshold sweep caught it as a one-point false-positive rebound at
        // 0.35. Strictness can only ever add, never subtract.
        let operator_bar = if self.min_score > 0.3 {
            self.min_score
        } else {
            0.0
        };
        // 按查询语言选底噪：英文短文本在该 embedder 下有 ~0.2–0.4 的固有余弦
        // （见 `semantic_min_score` 的实测），CJK 没有这个问题（见
        // `semantic_min_score_cjk` 的两条测试）。operator 严格度对两者都只加不减。
        let base = if has_cjk(&msg_lower) {
            self.semantic_min_score_cjk
        } else {
            self.semantic_min_score
        };
        let semantic_threshold = base.max(operator_bar);
        for (cmd_key, info) in commands {
            if scored.iter().any(|m| m.name == info.name) {
                continue;
            }
            // Description-only embedding: name/tags are curated literal
            // trigger surfaces already served by the keyword/CJK passes —
            // embedding them lets a single shared bigram ("存储" in a tag)
            // false-positive on unrelated queries. The description is
            // where paraphrase signal lives. Stopwords stripped on the
            // description side for the same glue-bigram reason as the
            // query (CJK runs survive — is_alphanumeric covers CJK).
            let desc_for_vec: String = info
                .description
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| !w.is_empty() && !STOPWORDS.contains(w))
                .collect::<Vec<_>>()
                .join(" ");
            let sim = hermix_intelligence::embedding::cosine_similarity(
                &query_vec,
                &embedder.embed_sync(&desc_for_vec),
            );
            // 沉淀产物同样降权，且同样在阈值判断之前 —— 向量层的底噪本来就
            // 是"恰好过线"的性质，降权后应当直接落选而不是占名额。
            let auto = is_auto_generated(info);
            let sim = if auto {
                sim * AUTO_SKILL_SCORE_FACTOR
            } else {
                sim
            };
            // 中文向量阈值（0.18）低于沉淀产物的门槛下限（0.3），所以这里也
            // 要抬——否则中文查询下沉淀技能仍能从最松的那道门进来。
            let threshold = if auto {
                semantic_threshold.max(AUTO_SKILL_MIN_SCORE)
            } else {
                semantic_threshold
            };
            if sim < threshold {
                continue;
            }
            let Some(body) = self.load_body(&info.skill_md_path) else {
                continue;
            };
            scored.push(SkillMatch {
                command_key: cmd_key.clone(),
                name: info.name.clone(),
                description: info.description.clone(),
                score: sim,
                body,
                matched_on: "vector".into(),
                allowed_tools: info.allowed_tools.clone(),
            });
        }

        // Sort by score descending, take top N
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(self.max_matches);
        scored
    }

    /// Load a skill body with progressive-disclosure degradation.
    /// - Body within `max_body_lines`: full body.
    /// - Over cap: section SKELETON (every heading + the first content
    ///   line under it) + a footer pointing at `skill_view`. We never
    ///   inject a truncated half-article — a cut-off instruction block
    ///   reads as complete and gets followed halfway.
    /// Returns `None` when the file is unreadable (caller skips the skill).
    fn load_body(&self, skill_md_path: &Path) -> Option<String> {
        let content = std::fs::read_to_string(skill_md_path).ok()?;
        let (_, body) = crate::skill_orchestrator::parse_frontmatter(&content);
        let trimmed = body.trim();
        let total_lines = trimmed.lines().count();
        if total_lines <= self.max_body_lines {
            return Some(trimmed.to_string());
        }
        let mut out = String::new();
        let mut last_was_heading = false;
        for line in trimmed.lines() {
            let t = line.trim_end();
            if t.starts_with('#') {
                out.push_str(t);
                out.push('\n');
                last_was_heading = true;
            } else if last_was_heading && !t.trim().is_empty() {
                out.push_str("  ");
                out.push_str(t);
                out.push('\n');
                last_was_heading = false;
            }
        }
        out.push_str(&format!(
            "\n[Skeleton preview: full text is {total_lines} lines, over the {} line \
             injection cap — use skill_view to read the complete content before acting.]",
            self.max_body_lines
        ));
        Some(out)
    }

    /// Build the injection text for matched skills.
    /// Returns `None` if no skills matched. The returned text is suitable
    /// for appending to the system prompt.
    pub fn build_injection(&self, message: &str) -> Option<String> {
        self.build_injection_and_tools(message).map(|(text, _)| text)
    }

    /// Same injection text as [`Self::build_injection`], plus the union of
    /// the matched skills' `allowed-tools` (None when no matched skill
    /// declares any, or all declarations are empty). The agent loop stores
    /// this per turn and **subtracts** it from the dispatch surface —
    /// activation never expands what the session may call.
    pub fn build_injection_and_tools(
        &self,
        message: &str,
    ) -> Option<(String, Option<std::collections::HashSet<String>>)> {
        let matches = self.match_message(message);
        if matches.is_empty() {
            return None;
        }

        let mut parts = Vec::new();
        parts.push(
            "## Auto-Matched Skills (system-injected)\n\
             The following skills were automatically matched to the user's message. \
             Review their instructions and apply them if relevant to the current task.\n"
                .to_string(),
        );

        for m in &matches {
            parts.push(format!(
                "### {} (score: {:.2}, matched on: {})\n{}\n",
                m.name, m.score, m.matched_on, m.body
            ));
        }

        let mut tools: Option<std::collections::HashSet<String>> = None;
        for m in &matches {
            if let Some(list) = m.allowed_tools.as_ref() {
                if !list.is_empty() {
                    let set = tools.get_or_insert_with(std::collections::HashSet::new);
                    for t in list {
                        let t = t.trim();
                        if !t.is_empty() {
                            set.insert(t.to_string());
                        }
                    }
                }
            }
        }

        Some((parts.join("\n"), tools))
    }

    /// Find skills for a capability gap.
    /// When the LLM can't complete a task with the available tools, it can
    /// describe what it needs and this function returns skills that might
    /// help. This is the "find-skills 能力缺口兜底" from 铁律 agent_skills.
    pub fn find_skills_for_capability_gap(&self, description: &str) -> Vec<SkillMatch> {
        // Reuse the same matching logic — the "capability gap description"
        // is treated like a user message for matching purposes.
        self.match_message(description)
    }

    /// Build the find-skills response text for the LLM.
    /// Returns a JSON string listing matching skills with their names,
    /// descriptions, and command keys. The LLM can then use `skill_view`
    /// to inspect a skill's full content.
    pub fn find_skills_response(&self, description: &str) -> String {
        let matches = self.find_skills_for_capability_gap(description);
        if matches.is_empty() {
            return serde_json::json!({
                "found": false,
                "message": "No skills found matching the description. Try rephrasing or use skills_list to see all available skills.",
            })
            .to_string();
        }

        let results: Vec<serde_json::Value> = matches
            .iter()
            .map(|m| {
                serde_json::json!({
                    "name": m.name,
                    "command": m.command_key,
                    "description": m.description,
                    "score": m.score,
                    "matched_on": m.matched_on,
                })
            })
            .collect();

        serde_json::json!({
            "found": true,
            "count": results.len(),
            "skills": results,
            "hint": "Use skill_view with the skill name to see full instructions.",
        })
        .to_string()
    }
}

impl Default for SkillLoader {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Stopword filter (kept tiny on purpose — the auto-router is
// strict about activating skills, and any word on this list would
// false-positive virtually every skill with prose descriptions).
// ---------------------------------------------------------------------------

/// 沉淀产物的降权系数（`skills/auto/`）。
///
/// ## 为什么要有这个
///
/// `skills/auto/` 是 agent 自己写的：成功序列的骨架、多次执行的对比提炼。
/// 它有复用价值，但和手写技能有两点本质不同：
///
/// 1. **数量随使用线性增长**——跑几十个任务就多几十个技能，而手写技能是
///    人挑出来的少数几个；匹配池变大，误激活概率随之上升。
/// 2. **质量参差**——它是"这次跑通了"的记录，不是"这是最佳实践"的断言。
///
/// 降权让两者**同场不同价**：同一个查询命中时手写技能优先；只有沉淀技能
/// 真正相关且分数足够高时才进 top-N。0.75 的取法：手写 0.5 仍能压过沉淀
/// 0.6（→0.45），但沉淀 0.8（→0.6）依然能独立胜出——不把沉淀技能一棍打死，
/// 因为"下次复用"正是它存在的理由。
const AUTO_SKILL_SCORE_FACTOR: f32 = 0.75;

/// 沉淀产物的**门槛下限**（不参与软化）。
///
/// 手写技能在多词查询下可以用到 0.15 的软化阈值（保释义召回）；沉淀产物必须
/// 达到 0.3 —— 也就是"默认门槛、不软化"。两层治理缺一不可：只有降权时，
/// raw 0.25 → 0.1875 仍然越过 0.15 入池（实测），把 top-3 名额占掉。
const AUTO_SKILL_MIN_SCORE: f32 = 0.3;

/// `true` 当这个技能是 agent 自动沉淀的。
///
/// 同时判 `category` 与路径：`category` 来自 frontmatter（可能缺失或被手改），
/// 路径里的 `auto` 段则是 `skill_auto_create::AUTO_SKILL_DIR` 的硬约定。
/// 任一命中即算——宁可多降权一个手写技能，也不要漏掉一个沉淀产物。
fn is_auto_generated(info: &crate::skill_orchestrator::SkillCommandInfo) -> bool {
    let auto_dir = crate::skill_auto_create::AUTO_SKILL_DIR;
    info.category.eq_ignore_ascii_case(auto_dir)
        || info
            .skill_md_path
            .components()
            .any(|c| c.as_os_str() == auto_dir)
}

const STOPWORDS: &[&str] = &[
    // Pure function / grammatical glue — never a domain concept.
    "and", "the", "for", "with", "are", "that", "this",
    "your", "can", "use", "how", "what", "why", "who", "when",
    "from", "into", "have", "has", "had", "but", "not", "any",
    "all", "some", "out", "also", "just", "than", "then", "now",
    "tell", "give", "make", "about",
];

// ---------------------------------------------------------------------------
//Fuzzy matching helpers
// ---------------------------------------------------------------------------

/// Returns true if `needle`'s characters all appear in `haystack`,
/// in order, with adjacent matches no more than 5 characters apart.
/// This is the cheap fuzzy backbone the keyword pass falls back to
/// when exact word-overlap misses.
/// The 5-char gap budget is what separates a real fuzzy hit ("compres"
/// spans the inside of "compression") from a sparse false positive
/// ("world" picks up w/o/r/l/d from "review code for quality and best
/// practices" each separated by 2-12 unrelated characters).
/// Example: subsequence_match("compres", "context compression")
/// returns true; subsequence_match("momeression", "context
/// compression") returns false. It is intentionally *cheap* — no
/// Levenshtein, no embeddings — so it stays deterministic and
/// synchronous (the router runs on every turn).
/// Returns true if `needle`'s characters all appear **inside a
/// single word** of `haystack`, in order, with adjacent matches no
/// more than 3 characters apart.
/// Word-boundary split is what separates a real typo-bridge from
/// the historical false positive where `needle="world"` substring-
/// matched "review code for quality and best practices" (w / o / r /
/// l / d each picked up from unrelated words). Restricting the
/// search to one word at a time rejects that pattern while still
/// surfacing within-word partial-prefixes (`compres` → `compression`,
/// `memry` → `memory`).
pub fn subsequence_match(needle: &str, haystack: &str) -> bool {
    const MAX_GAP: usize = 3;

    let needle = needle.trim();
    if needle.is_empty() {
        return false;
    }
    if needle.len() < 3 {
        // Anything shorter than 3 chars trips false positives
        // everywhere ("is" subsequence-matches nearly every word).
        return false;
    }
    // Reject needles that consist only of punctuation —
    // they'd vacuously "match" everything because the loop
    // skips non-alphanumeric chars and there are zero
    // characters left to fail the iteration.
    if needle.chars().all(|c| !c.is_alphanumeric()) {
        return false;
    }

    // Try each word in the haystack independently. A real typo
    // bridge lives inside a single word; the sparse multi-word
    // case is exactly what we want to filter out.
    for word in haystack.split(|c: char| !c.is_alphanumeric()) {
        if word.is_empty() {
            continue;
        }
        if subsequence_match_within_word(needle, word, MAX_GAP) {
            return true;
        }
    }
    false
}

/// Internal: per-word subsequence check with a configurable gap.
fn subsequence_match_within_word(needle: &str, word: &str, max_gap: usize) -> bool {
    let w_chars: Vec<char> = word.chars().collect();
    let mut last_pos: Option<usize> = None;

    for nc in needle.chars() {
        if !nc.is_alphanumeric() {
            continue;
        }
        let start = last_pos.map(|p| p + 1).unwrap_or(0);
        let mut found = None;
        for (i, &hc) in w_chars.iter().enumerate().skip(start) {
            if hc.eq_ignore_ascii_case(&nc) {
                found = Some(i);
                break;
            }
        }
        let i = match found {
            Some(i) => i,
            None => return false,
        };
        if let Some(last) = last_pos {
            if i - last > max_gap {
                return false;
            }
        }
        last_pos = Some(i);
    }
    true
}

/// Returns a fuzzy score (0.0–1.0) for how well a user's message
/// tokens align with a skill description. We blend two cheap
/// signals so a richly-described skill beats a thinly-described
/// skill for the same query:
/// - exact-token-overlap (count of shared tokens / total message
/// tokens)
/// - subsequence coverage (fraction of message tokens with at
/// least one descriptor-character subsequence hit)
/// The blend weights are picked to favour the cheaper signal but
/// still reward descriptions that *contain* the user's intent even
/// when word boundaries differ (e.g. "memory" vs "memory-systems"
/// vs "persistent memory across sessions").
pub fn description_fuzzy_score(message_tokens: &[&str], description_lower: &str) -> f32 {
    if message_tokens.is_empty() {
        return 0.0;
    }

    // Whole-word membership check so a token like "compres" does
    // NOT count as a substring-hit inside "compression". The
    // `split_whitespace()` cost is one-shot — we cache the word set
    // for the whole token list iteration.
    let desc_words: std::collections::HashSet<&str> = description_lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();

    let mut exact_hits = 0usize;
    let mut subseq_hits = 0usize;

    for token in message_tokens {
        if desc_words.contains(*token) {
            exact_hits += 1;
        } else if subsequence_match(token, description_lower) {
            subseq_hits += 1;
        }
    }

    let exact_score = exact_hits as f32 / message_tokens.len() as f32;
    // A weighted-token score: an exact hit counts as 1.0, a
    // subsequence-only hit counts as 0.5 (still substantive but
    // not as good as a clean word overlap). The combined ratio
    // stays in 0.0–1.0. We then take the max with `exact_score`
    // so descriptions with strong whole-word matches always win.
    // Examples with default `min_score = 0.3`:
    // "compres" 1 token 1 subseq → 0.5/1 = 0.5  ✓ activate
    // "memory and compres" 3 tokens (memory exact + compres
    // subseq) → (1 + 0.5) / 3 ≈ 0.5 ✓ activate
    // "hello world" 2 tokens 1 subseq → 0.5 / 2 = 0.25 ✗ reject
    let weighted = exact_hits as f32 + subseq_hits as f32 * 0.5;
    let weighted_score = (weighted / message_tokens.len() as f32).min(1.0);

    exact_score.max(weighted_score)
}

// ---------------------------------------------------------------------------
// CJK (Chinese) trigger fallback
// ---------------------------------------------------------------------------

/// True if `s` contains any CJK ideograph (BMP Unified + Ext A).
/// Good enough for trigger-phrase routing; we don't need full
/// Unicode script detection.
fn has_cjk(s: &str) -> bool {
    s.chars().any(|c| {
        ('\u{4E00}'..='\u{9FFF}').contains(&c) || ('\u{3400}'..='\u{4DBF}').contains(&c)
    })
}

/// Extract 2-character CJK bigrams from `s`. Only runs of CJK
/// characters are bigrammed; ASCII/whitespace are skipped. This is
/// the classic cheap Chinese IR trick: it makes word-order
/// independent ("清理磁盘" vs "磁盘清理" both surface the 清理/磁盘
/// bigrams) and survives natural phrasing ("帮我清理一下电脑磁盘"
/// still contains the 清理/磁盘 bigrams).
fn cjk_bigrams(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if has_cjk(&chars[i].to_string()) {
            let start = i;
            let mut end = i + 1;
            while end < chars.len() && has_cjk(&chars[end].to_string()) {
                end += 1;
            }
            let run: Vec<char> = chars[start..end].to_vec();
            for w in run.windows(2) {
                out.push(w.iter().collect());
            }
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// Returns true if the user message's CJK bigrams overlap the
/// skill's curated CJK trigger `tags`/`name` by at least 2 distinct
/// bigrams.
/// We match against the skill's *tags* and *name* (hand-authored by
/// the skill author), NOT the whole description prose — this bounds
/// the false-positive surface to the tag vocabulary. The ≥2-bigram
/// threshold suppresses single-common-word false positives (e.g.
/// "分析一下这个项目" shares only the 分析 bigram with the
/// storage-analyzer skill, which is below the threshold and would
/// otherwise light it up).
fn cjk_bigram_hit(msg_lower: &str, name_lower: &str, tags_lower: &[String]) -> bool {
    if !has_cjk(msg_lower) {
        return false;
    }
    let msg_bigrams = cjk_bigrams(msg_lower);
    let mut consider: Vec<String> = tags_lower.to_vec();
    if has_cjk(name_lower) {
        consider.push(name_lower.to_string());
    }
    // Curated-tag substring match: the message contains a full trigger
    // phrase (e.g. "c盘", "清理磁盘"). This catches cases where
    // bigrams alone fall short — a single-CJK-char tail like "C盘"
    // yields no bigram, and a short phrase may share only one bigram
    // with a tag.
    for t in &consider {
        if has_cjk(t) && msg_lower.contains(t) {
            return true;
        }
    }
    // Otherwise require ≥2 shared bigrams to suppress single-common-
    // word false positives (e.g. "分析一下这个项目" shares only the
    // 分析 bigram with the storage skill).
    if msg_bigrams.len() < 2 {
        return false;
    }
    let mut shared: std::collections::HashSet<String> = std::collections::HashSet::new();
    for t in &consider {
        if !has_cjk(t) {
            continue;
        }
        for bg in cjk_bigrams(t) {
            if msg_bigrams.contains(&bg) {
                shared.insert(bg);
            }
        }
    }
    shared.len() >= 2
}



#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn setup_test_skills() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();

        // Create a few test skills. Tags are comma-separated inside
        // the inline list so the YAML parser actually splits them
        // (an unspaced single string inside `[]` collapses into one
        // tag, which doesn't reflect how real SKILL.md files look).
        let skills = [
            (
                "commit-message",
                "Format a Conventional Commit message",
                vec!["git", "commit", "conventional", "message", "format"],
                "# Commit Message Skill\n\nWhen writing a commit message, use the Conventional Commits format.",
            ),
            (
                "code-review",
                "Review code for quality and best practices",
                vec!["code", "review", "quality", "best-practices"],
                "# Code Review Skill\n\nReview the code for quality, security, and best practices.",
            ),
            (
                "pdf-extract",
                "Extract text and data from PDF files",
                vec!["pdf", "extract", "text", "data"],
                "# PDF Extract Skill\n\nUse read_pdf to extract text from PDF files.",
            ),
        ];

        for (name, desc, tags, body) in &skills {
            let skill_dir = dir.join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            let tags_inline = tags.join(", ");
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags_inline}]\n---\n{body}"
            );
            fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        }

        tmp
    }

    #[test]
    fn test_match_message_finds_relevant_skill() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("help me write a git commit message");
        assert!(!matches.is_empty());
        // The commit-message skill should be in the top matches
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"commit-message"));
    }

    #[test]
    fn test_match_message_returns_empty_for_irrelevant() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("hello world");
        // "hello" and "world" don't match any skill
        assert!(matches.is_empty());
    }

    #[test]
    fn test_match_message_respects_max_matches() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path()).with_max_matches(1);
        loader.scan();

        let matches = loader.match_message("code review commit message");
        assert!(!matches.is_empty());
        assert!(matches.len() <= 1);
    }

    #[test]
    fn test_build_injection_returns_none_when_no_match() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let injection = loader.build_injection("hello world");
        assert!(injection.is_none());
    }

    #[test]
    fn test_build_injection_returns_text_when_matched() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let injection = loader.build_injection("help me write a commit message");
        assert!(injection.is_some());
        let text = injection.unwrap();
        assert!(text.contains("Auto-Matched Skills"));
        assert!(text.contains("commit-message"));
        assert!(text.contains("Conventional Commits"));
    }

    #[test]
    fn test_find_skills_for_capability_gap() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.find_skills_for_capability_gap("I need to extract text from a PDF");
        assert!(!matches.is_empty());
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"pdf-extract"));
    }

    #[test]
    fn test_find_skills_response_json() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let response = loader.find_skills_response("I need to review code quality");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["found"], true);
        assert!(parsed["count"].as_u64().unwrap() > 0);
        assert!(parsed["skills"].is_array());
    }

    #[test]
    fn test_find_skills_response_empty() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let response = loader.find_skills_response("xyzzy qwerty");
        let parsed: serde_json::Value = serde_json::from_str(&response).unwrap();
        assert_eq!(parsed["found"], false);
    }

    #[test]
    fn test_match_on_tags() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "best-practices" is a tag on the code-review skill
        let matches = loader.match_message("show me best-practices for coding");
        assert!(!matches.is_empty());
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"code-review"));
    }

    #[test]
    fn test_score_threshold_filters_low_relevance() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path()).with_min_score(0.99);
        loader.scan();

        // Very high threshold — should filter out most matches
        let matches = loader.match_message("help me write a commit message about code review");
        // The score would be low because only a few tokens match each skill
        assert!(matches.is_empty() || matches.iter().all(|m| m.score >= 0.99));
    }

    #[test]
    fn test_empty_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("anything at all");
        assert!(matches.is_empty());
    }

    #[test]
    fn test_nonexistent_skills_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let nonexistent = tmp.path().join("nonexistent");
        let mut loader = SkillLoader::with_dir(&nonexistent);
        loader.scan();

        let matches = loader.match_message("anything");
        assert!(matches.is_empty());
    }

    // --- CJK (Chinese) keyword auto-routing ---

    /// Build a temp skills dir with two skills that carry Chinese
    /// trigger tags: a storage-analyzer-like skill (磁盘/缓存/存储
    /// triggers) and a ponytail-like skill (懒人/最简/写代码 triggers).
    fn setup_cjk_test_skills() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let skills = [
            (
                "storage-analyzer",
                "Windows read-only storage analysis assistant",
                vec!["清理磁盘", "磁盘清理", "C盘", "空间不够", "清缓存", "存储分析"],
                "# Storage Analyzer\n\nScan disk usage and classify junk.",
            ),
            (
                "ponytail",
                "Force the laziest solution that works",
                vec!["懒人模式", "最简方案", "少写代码", "最小实现", "过度工程"],
                "# Ponytail\n\nWrite the minimal code that works.",
            ),
        ];
        for (name, desc, tags, body) in &skills {
            let skill_dir = tmp.path().join(name);
            fs::create_dir_all(&skill_dir).unwrap();
            let tags_inline = tags.join(", ");
            let content = format!(
                "---\nname: {name}\ndescription: {desc}\ntags: [{tags_inline}]\n---\n{body}"
            );
            fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        }
        tmp
    }

    #[test]
    fn cjk_match_storage_chinese_query() {
        let tmp = setup_cjk_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // Natural phrasing with extra filler words still fires.
        let matches = loader.match_message("帮我清理一下C盘");
        assert!(!matches.is_empty(), "Chinese storage query should match");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"storage-analyzer"), "got: {names:?}");
    }

    #[test]
    fn cjk_match_ponytail_chinese_query() {
        let tmp = setup_cjk_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("用最简方案写这段代码");
        assert!(!matches.is_empty(), "Chinese ponytail query should match");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"ponytail"), "got: {names:?}");
    }

    #[test]
    fn cjk_no_false_positive_on_common_words() {
        let tmp = setup_cjk_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "分析一下这个项目" only shares the single 分析 bigram with
        // the storage skill (below the ≥2 threshold) → must NOT fire.
        let matches = loader.match_message("分析一下这个项目");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(
            !names.contains(&"storage-analyzer"),
            "common-word query must not false-trigger storage: {names:?}"
        );

        // "帮我存储这个文件" — 存储 appears inside 存储分析 but as a
        // single bigram, below threshold → must NOT fire.
        let matches2 = loader.match_message("帮我存储这个文件");
        let names2: Vec<&str> = matches2.iter().map(|m| m.name.as_str()).collect();
        assert!(
            !names2.contains(&"storage-analyzer"),
            "store-file query must not false-trigger storage: {names2:?}"
        );
    }

    //fuzzy matching helpers ---

    #[test]
    fn p2_fuzzy_subsequence_matches_within_word() {
        // "compres" subsequence-matches inside "compression" itself —
        // we want fuzzy to bridge drop-final-letter typos without
        // crossing whitespace into unrelated words.
        assert!(subsequence_match("compres", "compression"));
        // "memry" — needle chars all appear in order inside
        // "memory" (m-e-m-r-y vs m-e-m-o-r-y, the missing 'o'
        // is just skipped over via the gap budget).
        assert!(subsequence_match("memry", "memory"));
    }

    #[test]
    fn p2_fuzzy_subsequence_rejects_substitution_typos() {
        // subsequence_match expects every needle char IN ORDER.
        // A character-substitution typo (e → o) is not a match —
        // that's Levenshtein territory, out of scope for our
        // cheap fuzzy pass.
        assert!(!subsequence_match("memery", "memory"));
    }

    #[test]
    fn p2_fuzzy_subsequence_finds_match_in_any_word() {
        // With the per-word split, "compres" finds the matching
        // "compression" word even when it's not the first word in
        // the description. This is the whole reason we restrict
        // to within-word: the *first* candidate word ("practices")
        // would have trapped a naive single-pass matcher if `c`
        // wasn't the trailing character gap.
        assert!(subsequence_match(
            "compres",
            "best practices for lossy compression of conversation history"
        ));
    }

    #[test]
    fn p2_fuzzy_subsequence_rejects_sparse_multi_word_hit() {
        // The historical false positive: "world" → "review code
        // for quality and best practices". Each char of `world`
        // lives in a different word; the per-word restriction
        // rejects the pattern even when no single word starts
        // with `w`.
        assert!(!subsequence_match(
            "world",
            "review code for quality and best practices"
        ));
    }

    #[test]
    fn p2_fuzzy_subsequence_requires_all_chars_in_order() {
        // "momeression" — same chars scrambled, must NOT match.
        assert!(!subsequence_match("momeression", "context compression"));
    }

    #[test]
    fn p2_fuzzy_subsequence_rejects_short_needles() {
        // 2-char needles trip false positives everywhere.
        assert!(!subsequence_match("is", "this is a sentence"));
        assert!(!subsequence_match("", "anything"));
    }

    #[test]
    fn p2_fuzzy_subsequence_skips_needle_punctuation() {
        // Punctuation in the needle is ignored at the char-iteration
        // level, so the matcher operates on alphanumeric runs only.
        // "h-y-p-e-n" effectively becomes "hypen" — a full ordered
        // hit inside the single haystack word "hypen-ated".
        assert!(subsequence_match("h-y-p-e-n", "hypen-ated real word"));
    }

    #[test]
    fn p2_fuzzy_score_single_subseq_hit_passes_default_threshold() {
        // "compres" against "compression of conversation history"
        // (within-word, no cross-word gap):
        // → exact_hits = 0 (no whole-word `compres` in desc)
        // → subseq_hits = 1, len = 1
        // → exact_score = 0, weighted = 0.5, weighted_score = 0.5/1 = 0.5
        // → max(0, 0.5) = 0.5 ≥ default `min_score` of 0.3
        let score = description_fuzzy_score(
            &["compres"],
            "compression of conversation history",
        );
        assert!(
            (score - 0.5).abs() < f32::EPSILON,
            "expected 0.5, got {score}"
        );
    }

    #[test]
    fn p2_fuzzy_score_sparse_subseq_across_tokens_stays_below_threshold() {
        //The historical false positive that bit us before :
        // "world" subsequence-matches "review code for quality and
        // best practices" (w-o-r-l-d all scattered around), but
        // across two message tokens the discounted score must stay
        // under the default `min_score = 0.3` so we don't surface
        // an irrelevant skill.
        let score = description_fuzzy_score(
            &["hello", "world"],
            "review code for quality and best practices",
        );
        assert!(
            score < 0.3,
            "sparse subseq across 2 tokens should not pass 0.3, got {score}"
        );
    }

    #[test]
    fn p2_fuzzy_score_exact_token_dominates_fuzzy() {
        // Exact token overlap on 1 of 2 tokens → 0.5 ≥ any subseq path.
        let score = description_fuzzy_score(
            &["commit", "kangaroo"],
            "format a conventional commit message",
        );
        assert!(score >= 0.5, "exact hit should dominate, got {score}");
    }

    //body line-cap ---

    #[test]
    fn p2_body_cap_truncates_long_skills_without_erroring() {
        let tmp = tempfile::tempdir().unwrap();
        let long_dir = tmp.path().join("long-skill");
        std::fs::create_dir_all(&long_dir).unwrap();

        // Build a 200-line body — well past `with_max_body_lines(5)`.
        let mut body = String::from("---\nname: long-skill\ndescription: A long skill\ntags: [long]\n---\n# Long\n");
        for i in 0..200 {
            body.push_str(&format!("line {i}\n"));
        }
        std::fs::write(long_dir.join("SKILL.md"), body).unwrap();

        let mut loader = SkillLoader::with_dir(tmp.path())
            .with_max_matches(1)
            .with_max_body_lines(5);
        loader.scan();

        let matches = loader.match_message("show me a long skill");
        assert_eq!(matches.len(), 1);
        let m = &matches[0];

        // Over-cap body degrades to a skeleton: footer points at
        // `skill_view` (never a silent half-article).
        assert!(
            m.body.contains("Skeleton preview"),
            "expected skeleton footer, got body tail: …{}",
            &m.body[m.body.len().saturating_sub(120)..]
        );
        assert!(m.body.contains("use skill_view"));
        // Skeleton keeps headings + the first content line…
        assert!(m.body.contains("# Long"));
        assert!(m.body.contains("line 0"));
        // …but not deep-section lines (that's the whole point).
        assert!(!m.body.contains("line 199"));
    }

    // --- Vector recall (semantic layer) ---

    /// A CJK paraphrase that shares NO whitespace tokens and NO CJK
    /// trigger tags with the skill — only the keyword passes miss it.
    fn setup_semantic_test_skill() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("impeccable-design");
        fs::create_dir_all(&skill_dir).unwrap();
        // Deliberately NO Chinese tags: keyword/CJK passes cannot fire.
        let content = "---\nname: impeccable-design\ndescription: 界面与视觉设计规范，覆盖界面设计、配色方案、页面美化与排版布局。\ntags: [design, builtin]\n---\n# 设计规范\n\n按纪律执行。\n";
        fs::write(skill_dir.join("SKILL.md"), content).unwrap();
        tmp
    }

    /// 沉淀产物（`skills/auto/`）降权的三条性质。
    ///
    /// 动机：auto 技能由 run loop 自动写入，数量随使用线性增长，质量参差；
    /// 若不降权，它们会和手写技能抢同一个 top-N 名额，把误激活率重新推高
    /// （阈值扫描刚把它从 3/10 修到 0/10）。
    fn seed_manual_and_auto(dir: &std::path::Path) {
        // 同样的 description 与 tag —— 唯一差别是分类目录。
        for (category, name) in [("manual", "deploy"), ("auto", "deploy-auto")] {
            let d = dir.join(category).join(name);
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(
                d.join("SKILL.md"),
                format!(
                    "---\nname: {name}\ndescription: Deploy the application to production servers.\n\
                     tags: [deploy, production]\n---\n# {name}\n"
                ),
            )
            .unwrap();
        }
    }

    #[test]
    fn hand_written_skill_outranks_an_equally_matching_auto_skill() {
        let tmp = tempfile::tempdir().unwrap();
        seed_manual_and_auto(tmp.path());
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("deploy the application to production");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(names.contains(&"deploy"), "手写技能必须命中: {names:?}");
        assert_eq!(
            matches[0].name, "deploy",
            "同分时手写技能必须排第一（沉淀产物降权 0.75）: {names:?}"
        );
        // 被降权的那个分数必须严格更低。
        let auto = matches.iter().find(|m| m.name == "deploy-auto");
        if let Some(auto) = auto {
            assert!(
                auto.score < matches[0].score,
                "沉淀技能分数 {} 必须低于手写 {}",
                auto.score,
                matches[0].score
            );
        }
    }

    /// 降权不是封杀：只有沉淀技能相关时，它照样要能被激活——
    /// "下次复用"正是自动沉淀存在的理由。
    #[test]
    fn an_auto_skill_alone_still_activates() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("auto").join("rotate-logs");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: rotate-logs\ndescription: Rotate application logs and archive them.\n\
             tags: [logs, rotate]\n---\n# rotate-logs\n",
        )
        .unwrap();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("rotate the logs please");
        assert!(
            matches.iter().any(|m| m.name == "rotate-logs"),
            "唯一的沉淀技能必须仍可激活: {:?}",
            matches.iter().map(|m| m.name.as_str()).collect::<Vec<_>>()
        );
    }

    /// 降权后达不到阈值的沉淀技能必须**落选**，而不是以低分占住 top-N 名额。
    #[test]
    fn a_weak_auto_match_is_dropped_by_the_discount() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("auto").join("weak");
        std::fs::create_dir_all(&d).unwrap();
        // 只有 1/4 的词命中，raw 0.25 → 降权 0.1875 < 0.3 阈值。
        std::fs::write(
            d.join("SKILL.md"),
            "---\nname: weak\ndescription: archive servers quarterly\n---\n# weak\n",
        )
        .unwrap();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        let matches = loader.match_message("archive something unrelated entirely");
        assert!(
            !matches.iter().any(|m| m.name == "weak"),
            "降权后低于阈值的沉淀技能不应入池: {:?}",
            matches.iter().map(|m| (m.name.as_str(), m.score)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn vector_recall_matches_cjk_paraphrase_keyword_misses() {
        let tmp = setup_semantic_test_skill();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "界面配色" shares 界面/配 trigrams with the description but has
        // no literal tag/name hit (tags are English) → only the vector
        // layer can surface this skill.
        let matches = loader.match_message("帮我美化一下界面配色");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(
            names.contains(&"impeccable-design"),
            "paraphrase should match via vector recall, got: {names:?}"
        );
        let m = matches.iter().find(|m| m.name == "impeccable-design").unwrap();
        assert_eq!(m.matched_on, "vector");
    }

    #[test]
    fn vector_recall_does_not_fire_on_unrelated_query() {
        let tmp = setup_semantic_test_skill();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // Unrelated query must NOT surface the design skill.
        let matches = loader.match_message("帮我清理C盘垃圾文件");
        let names: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
        assert!(
            !names.contains(&"impeccable-design"),
            "unrelated query must not match: {names:?}"
        );
    }

    #[test]
    fn keyword_match_score_wins_over_vector() {
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();

        // "commit message" keyword-matches commit-message; the vector
        // layer must not duplicate it or overwrite matched_on.
        let matches = loader.match_message("help me write a git commit message");
        let cm = matches.iter().find(|m| m.name == "commit-message").unwrap();
        assert_ne!(cm.matched_on, "vector");
        assert_eq!(
            matches.iter().filter(|m| m.name == "commit-message").count(),
            1,
            "dedupe: one entry per skill"
        );
    }

    #[test]
    fn cosine_similarity_basics() {
        use hermix_intelligence::embedding::cosine_similarity;
        assert!((cosine_similarity(&[1.0, 0.0], &[1.0, 0.0]) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 2.0]), 0.0, "dim mismatch");
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0, "zero norm");
    }

    #[test]
    fn build_injection_and_tools_surfaces_allowed_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        let skill_dir = dir.join("pdf-scope");
        fs::create_dir_all(&skill_dir).unwrap();
        fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf-scope\ndescription: Extract PDF text safely\n\
             allowed-tools: [read_file, read_pdf]\n---\n# PDF Scope\nUse read_pdf only.",
        )
        .unwrap();

        let mut loader = SkillLoader::with_dir(dir);
        loader.scan();
        let (block, tools) = loader
            .build_injection_and_tools("extract text from this pdf")
            .expect("skill should match");
        assert!(block.contains("PDF Scope"));
        let tools = tools.expect("allowed-tools must surface");
        assert!(tools.contains("read_file"));
        assert!(tools.contains("read_pdf"));
        assert!(!tools.contains("terminal"));
    }

    #[test]
    fn build_injection_and_tools_is_none_without_declaration() {
        // Skills with no `allowed-tools` must not impose any scope.
        let tmp = setup_test_skills();
        let mut loader = SkillLoader::with_dir(tmp.path());
        loader.scan();
        let (_block, tools) = loader
            .build_injection_and_tools("write a commit message")
            .expect("skill should match");
        assert!(tools.is_none(), "no declaration → no restriction");
    }

    /// 缓存命中与失效：同目录复用同一个 loader；新增技能目录必须触发重扫。
    #[test]
    fn cached_loader_reuses_and_invalidates_on_new_skill() {
        let tmp = tempfile::tempdir().unwrap();
        let first = cached_loader(tmp.path());
        let again = cached_loader(tmp.path());
        assert!(std::sync::Arc::ptr_eq(&first, &again), "同目录应命中缓存");

        let dir = tmp.path().join("new-skill");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: new-skill\ndescription: freshly added probe\ntags: [probe]\n---\n# New\n\nbody\n",
        )
        .unwrap();

        let after = cached_loader(tmp.path());
        assert!(
            !std::sync::Arc::ptr_eq(&first, &after),
            "新增技能后必须重扫（否则新技能永远进不来）"
        );
        assert!(
            after
                .match_message("freshly added probe")
                .iter()
                .any(|m| m.name == "new-skill"),
            "重扫后新技能应可匹配"
        );
    }

    /// 探针（默认 ignore）：量 `with_dir + scan()` 的真实成本。
    ///
    /// 用来决定 P1-9「每轮扫描」值不值得加缓存。缓存方案会让"改技能描述后
    /// 立即生效"退化成"要等 TTL/重启"，是否划算取决于这个数字 —— 先量再改，
    /// 不靠猜。
    #[test]
    #[ignore = "手动探针，不是回归断言"]
    fn probe_scan_cost() {
        let tmp = tempfile::tempdir().unwrap();
        for i in 0..30 {
            let dir = tmp.path().join(format!("skill-{i:02}"));
            fs::create_dir_all(&dir).unwrap();
            let content = format!(
                "---\nname: skill-{i}\ndescription: 第 {i} 个探针技能，用来量测扫描成本\ntags: [probe, perf]\n---\n# Skill {i}\n\n正文若干行。\n{}",
                "细节行\n".repeat(40)
            );
            fs::write(dir.join("SKILL.md"), content).unwrap();
        }
        // 预热一次：首扫含目录项缓存冷启动，不代表稳态。
        let mut warm = SkillLoader::with_dir(tmp.path());
        warm.scan();

        let start = std::time::Instant::now();
        for _ in 0..50 {
            let mut loader = SkillLoader::with_dir(tmp.path());
            loader.scan();
        }
        let per_call = start.elapsed() / 50;
        println!("with_dir+scan（30 个技能）× 50 次：单次 {per_call:?}");
    }
}

