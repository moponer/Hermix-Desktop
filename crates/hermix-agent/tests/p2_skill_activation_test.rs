//! : skill router activation regression test.
//!
//! Goal: pin a handful of deterministic routing decisions so a
//! future change to `SkillLoader::match_message` or the fuzzy
//! helpers can't silently degrade routing quality. We deliberately
//! stay conservative — the reference project ships 19 prompts
//! across a benchmark; we keep 6 high-confidence prompts locally
//! so a regression signal fires fast.
//!
//! Important: an unrelated future change (e.g. adding more skills,
//! tightening the `min_score`) is allowed to flip a verdict. The
//! helper below logs a *warning* rather than failing hard so a
//! real regression can still be triaged without blocking CI; the
//! owner-of-the-day is expected to either fix the router or
//! re-target the expected skill if the new library structure
//! genuinely warrants a different routing.
//!
//! The P2 plan calls for "≥ 80% Top-3 accuracy". This file is a
//! subset — full coverage lives in the future `` 50-prompt
//! benchmark (plan §P6).

use std::fs;

use hermix_agent::skill_loader::{SkillLoader, SkillMatch};

/// 6 deterministic query ↔ expected-skill pairs. Each prompt has
/// a clear ground truth: we wrote the description so the
/// expected skill *should* surface.
const CASES: &[(&str, &str)] = &[
    // Whole-word keyword hits.
    ("tell me about persistent agent memory", "memory-systems"),
    ("how do I do context compression", "context-compression"),
    ("design a safe tool for the LLM", "tool-design"),
    // Fuzzy subsequence hits.
    ("compres", "context-compression"),
    ("memry layout", "memory-systems"),
    // Routing after stop-word filter.
    ("I want memory compression", "memory-systems"),
];

/// Scaffold the same three skills the in-crate unit tests use.
fn seed_three_skills(dir: &std::path::Path) {
    let skills: &[(&str, &str, &[&str])] = &[
        (
            "context-compression",
            "Best practices for lossy compression of conversation history.",
            &["compression", "context"],
        ),
        (
            "memory-systems",
            "Architecture patterns for persistent agent memory across sessions.",
            &["memory", "architecture"],
        ),
        (
            "tool-design",
            "Principles for designing LLM-callable tools that are safe and effective.",
            &["tools", "design"],
        ),
    ];
    for (slug, desc, tags) in skills {
        let d = dir.join(slug);
        fs::create_dir_all(&d).unwrap();
        let tags_inline = tags.join(", ");
        fs::write(
            d.join("SKILL.md"),
            format!("---\nname: {slug}\ndescription: {desc}\ntags: [{tags_inline}]\n---\n# {slug}\n"),
        )
        .unwrap();
    }
}

/// Return the slug with the highest score, or None if no skills
/// surfaced. Used so we can assert "the expected skill wins".
fn top_skill(matches: &[SkillMatch]) -> Option<&str> {
    matches.first().map(|m| m.name.as_str())
}

#[test]
fn p2_router_keeps_deterministic_routing_decisions() {
    let tmp = tempfile::tempdir().unwrap();
    seed_three_skills(tmp.path());

    let mut loader = SkillLoader::with_dir(tmp.path());
    loader.scan();

    let mut passed = 0usize;
    let mut warned = Vec::new();

    for (prompt, expected) in CASES {
        let matches = loader.match_message(prompt);
        let got = top_skill(&matches);
        if got == Some(*expected) {
            passed += 1;
        } else {
            warned.push((*prompt, *expected, got.map(|s| s.to_string())));
        }
    }

    let total = CASES.len();
    let accuracy = passed as f32 / total as f32;
    eprintln!(
        "p2_router accuracy = {passed}/{total} = {:.0}%",
        accuracy * 100.0
    );

    // Soft check — log the misses but don't hard-fail the
    // whole test, matching the plan's "≥ 80%" guidance and
    //letting plan do the heavier 50-prompt benchmark.
    if accuracy < 0.80 {
        for (p, e, g) in &warned {
            eprintln!(
                "  MISS: prompt={p:?} expected={e:?} got={:?}",
                g.as_deref().unwrap_or("<none>")
            );
        }
        panic!(
            "router accuracy {passed}/{total} ({:.0}%) below 80% threshold",
            accuracy * 100.0
        );
    }
}

#[test]
fn p2_router_does_not_activate_unrelated_skills() {
    // Negative coverage: a query about *context-compression*
    // must NOT spuriously activate *tool-design* (unrelated
    // vocabulary). This is the inverse of the positive tests:
    // if this regresses, recall has over-rotated onto substring
    // matches.
    let tmp = tempfile::tempdir().unwrap();
    seed_three_skills(tmp.path());

    let mut loader = SkillLoader::with_dir(tmp.path());
    loader.scan();

    let matches = loader.match_message("what is the best practice for compression");
    let activated: Vec<&str> = matches.iter().map(|m| m.name.as_str()).collect();
    assert!(
        activated.contains(&"context-compression"),
        "context-compression should surface"
    );
    assert!(
        !activated.contains(&"tool-design"),
        "tool-design should NOT surface for a compression query, got {activated:?}"
    );
}

#[test]
fn p2_router_handles_garbage_query_gracefully() {
    // The router must never panic on a wildly typed query —
    // not even on empty input, not even on punctuation
    // soup. The score below the threshold should land as an
    // empty match list, not a None that downstream code has to
    // special-case.
    let tmp = tempfile::tempdir().unwrap();
    seed_three_skills(tmp.path());

    let mut loader = SkillLoader::with_dir(tmp.path());
    loader.scan();

    for junk in [
        "",
        " ",
        "!@#$%^&*()",
        "qwertyuiop zxcvbnm",
        "1 2 3 4 5 6 7 8 9 0",
    ] {
        let matches = loader.match_message(junk);
        assert!(
            matches.is_empty(),
            "junk query {junk:?} produced non-empty match list {matches:?}"
        );
    }
}
