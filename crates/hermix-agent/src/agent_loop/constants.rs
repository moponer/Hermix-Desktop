// ---------------------------------------------------------------------------
// Prompt constants for memory + skill review (moved from agent_loop.rs
// 2026-07-06, God Object refactor Phase 1)
// ---------------------------------------------------------------------------

/// Python `AIAgent._MEMORY_REVIEW_PROMPT` / `_SKILL_REVIEW_PROMPT` / `_COMBINED_REVIEW_PROMPT` (v2026.4.16)
pub(crate) const MEMORY_REVIEW_PROMPT: &str = "Review the conversation above and consider saving to memory if appropriate.\n\n\
Focus on:\n\
1. Has the user revealed things about themselves — their persona, desires, preferences, or personal details worth remembering?\n\
2. Has the user expressed expectations about how you should behave, their work style, or ways they want you to operate?\n\n\
If something stands out, save it using the memory tool. \
If nothing is worth saving, just say 'Nothing to save.' and stop.";

pub(crate) const SKILL_REVIEW_PROMPT: &str =
    "Review the conversation above and consider saving or updating a skill if appropriate.\n\n\
Focus on: was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome?\n\n\
**P0-1 (auto-skill learning loop)**: when you decide a skill is worth saving, \
prefer `skill_manage(action='auto_extract_from_conversation')` over `auto_create`. \
It enforces a quality gate (name 3–80 chars, summary >10 chars and non-placeholder, \
≥1 step) and writes SKILL.md frontmatter so the orchestrator picks the skill up \
cleanly. It also dedupes — if a skill with the same name exists, it updates instead \
of creating a duplicate.\n\n\
Provide a `name` (kebab-case slug, 3–80 chars), a `summary` (one paragraph, >10 \
chars), an ordered `steps` array (each step a concrete actionable instruction), \
and optionally a `rationale` explaining why this is worth remembering next time. \
If a relevant skill with the same name already exists, the dedup logic will route \
your call to an update automatically — just call `auto_extract_from_conversation`.\n\n\
If nothing is worth saving, just say 'Nothing to save.' and stop.";

pub(crate) const COMBINED_REVIEW_PROMPT: &str = "Review the conversation above and consider two things:\n\n\
**Memory**: Has the user revealed things about themselves — their persona, \
desires, preferences, or personal details? Has the user expressed expectations \
about how you should behave, their work style, or ways they want you to operate? \
If so, save using the memory tool.\n\n\
**Skills**: Was a non-trivial approach used to complete a task that required trial \
and error, or changing course due to experiential findings along the way, or did \
the user expect or desire a different method or outcome? **Prefer \
`skill_manage(action='auto_extract_from_conversation')`** — it enforces a quality \
gate, writes SKILL.md frontmatter, and dedupes against existing skills by name. \
Pass `name` (3–80 chars), `summary` (>10 chars), `steps` (≥1), and optionally \
`rationale`.\n\n\
Only act if there's something genuinely worth saving. \
If nothing stands out, just say 'Nothing to save.' and stop.";
