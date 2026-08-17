//! /T2 integration test.
//!
//! Goal: verify that the `SkillOrchestrator` can scan all 17 skills
//! from the [Agent-Skills-for-Context-Engineering](https://github.com/muratcankoylan/Agent-Skills-for-Context-Engineering)
//! reference project verbatim. We DON'T clone the repo — instead we
//! scaffold 17 mock `SKILL.md` files in a `tempdir`, each with the
//! same shape (`name`/`description`/`version`/`tags` frontmatter)
//! the open-standard skill files use, and assert that:
//!
//!   1. All 17 commands are registered.
//!   2. Slash-command resolution works for every skill by name and
//!      by hyphen ↔ underscore.
//!   3. Description-roundtrip preserves the original ASCII content
//!      (the orchestrator stores it as a fallback when the LLM
//!      needs a hint for routing).
//!
//! This test lives separately from the agent crate's unit tests
//! because it exercises the scanning path through the public API
//! (no internal `skill_orchestrator.rs` access required), which is
//! the same path the production agent exercises at session start.
//!
//! Status: (skill scaffolding), (validation).
//! (Chinese translation) is intentionally deferred — see plan doc.

use std::fs;
use std::path::Path;

use hermix_agent::skill_orchestrator::SkillOrchestrator;

/// The 17 skills from the Agent-Skills-for-Context-Engineering
/// reference project. Keep this list in lockstep with the README
/// upstream; if upstream adds one, add a row here too.
///
/// We store (slug, description, category) tuples; the body is
/// minimal — the orchestrator only needs the frontmatter to
/// register a slash command. The description is the routing
/// signal used by `skill_loader::match_skills_for_query` and
/// by P2's semantic router.
const SKILLS: &[(&str, &str, &str)] = &[
    (
        "context-fundamentals",
        "Foundational concepts for working with LLM context windows.",
        "fundamentals",
    ),
    (
        "context-degradation",
        "Detect and mitigate attention dilution, lost-in-middle, and context poisoning.",
        "degradation",
    ),
    (
        "context-compression",
        "Best practices for lossy compression of conversation history.",
        "compression",
    ),
    (
        "context-optimization",
        "Compression, masking, and caching strategies to maximize usable context.",
        "optimization",
    ),
    (
        "multi-agent-patterns",
        "Collaboration patterns for orchestrator + worker agent topologies.",
        "orchestration",
    ),
    (
        "long-horizon-prompting",
        "Bootstrapping prompts for autonomous agents that run for many turns.",
        "prompting",
    ),
    (
        "memory-systems",
        "Architecture patterns for persistent agent memory across sessions.",
        "memory",
    ),
    (
        "tool-design",
        "Principles for designing LLM-callable tools that are safe and effective.",
        "tools",
    ),
    (
        "filesystem-context",
        "Offload large contexts to files and re-discover them on demand.",
        "context",
    ),
    (
        "hosted-agents",
        "Patterns for hosted / managed agent runtimes.",
        "orchestration",
    ),
    (
        "latent-briefing",
        "KV-cache-aware briefing from orchestrator to worker to share context.",
        "orchestration",
    ),
    (
        "evaluation",
        "Evaluation framework — deterministic + LLM-as-judge for harnesses.",
        "evaluation",
    ),
    (
        "advanced-evaluation",
        "Pairwise / bootstrap / Wilcoxon statistical tests for agent evaluation.",
        "evaluation",
    ),
    (
        "harness-engineering",
        "Five-component harness blueprint — instructions / constraints / feedback / memory / orchestration.",
        "harness",
    ),
    (
        "self-improvement-loops",
        "Failure-driven harness self-editing and meta-harness search.",
        "evolution",
    ),
    (
        "project-development",
        "End-to-end LLM project development workflow.",
        "methodology",
    ),
    (
        "bdi-mental-states",
        "Formal Belief / Desire / Intention mental-state model for agents.",
        "cognition",
    ),
];

/// Scaffold the 17 SKILL.md files into `skills_dir`. Matches the
/// shape used by the open-standard agentskills.io frontmatter that
/// the upstream project publishes.
fn scaffold_skills(skills_dir: &Path) {
    for (slug, description, category) in SKILLS {
        let skill_dir = skills_dir.join(slug);
        fs::create_dir_all(&skill_dir).expect("create skill dir");

        // The slug is also the frontmatter `name` field; that's
        // what real upstream SKILL.md files look like and it's how
        // `SkillFrontmatter::name` ends up matching the directory
        // slug in our scanner.
        let body = format!(
            "---\nname: {slug}\ndescription: {description}\ncategory: {category}\n\
             version: 1.0.0\nlicense: MIT\ntags:\n  - {category}\n---\n\n\
             # {slug}\n\n{description}\n",
        );

        fs::write(skill_dir.join("SKILL.md"), body).expect("write SKILL.md");
    }
}

#[test]
fn p1_orchestrator_scans_all_17_skills() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold_skills(tmp.path());

    let mut orch = SkillOrchestrator::new(tmp.path());
    orch.scan_skill_commands();

    assert_eq!(
        orch.get_commands().len(),
        SKILLS.len(),
        "expected {} commands, got {}",
        SKILLS.len(),
        orch.get_commands().len()
    );

    // Every slug should be resolvable as a slash command.
    for (slug, _desc, _cat) in SKILLS {
        let cmd = format!("/{slug}");
        assert!(
            orch.get_commands().contains_key(&cmd),
            "missing slash command {cmd}; have {:?}",
            orch.get_commands().keys().collect::<Vec<_>>()
        );
    }
}

#[test]
fn p1_orchestrator_preserves_descriptions_for_routing() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold_skills(tmp.path());

    let mut orch = SkillOrchestrator::new(tmp.path());
    orch.scan_skill_commands();

    // The description is the primary signal P2's router matches
    // against `user_message`. If we lose it during ingestion,
    // automation drops from Top-3 93% → ~0%.
    for (slug, description, _cat) in SKILLS {
        let cmd = format!("/{slug}");
        let info = orch.get_commands().get(&cmd).expect("command");
        assert_eq!(
            info.description, *description,
            "description for {slug} was re-written during ingest"
        );
    }
}

#[test]
fn p1_orchestrator_resolves_underscore_and_hyphen_interchangeably() {
    let tmp = tempfile::tempdir().expect("tempdir");
    scaffold_skills(tmp.path());

    let mut orch = SkillOrchestrator::new(tmp.path());
    orch.scan_skill_commands();

    // Spot-check three skills from different categories so a
    // hard-coded mismatch in `slugify_skill_name` doesn't slip
    // through. We're not checking all 17 — three is enough to
    // expose a regression in hyphen normalization.
    for slug in ["context-degradation", "memory-systems", "bdi-mental-states"] {
        let underscore = slug.replace('-', "_");
        let cmd = format!("/{slug}");
        assert_eq!(
            orch.resolve_skill_command_key(slug).as_deref(),
            Some(cmd.as_str()),
            "hyphenated key {slug} should resolve"
        );
        assert_eq!(
            orch.resolve_skill_command_key(&underscore).as_deref(),
            Some(cmd.as_str()),
            "underscored key {underscore} should also resolve"
        );
    }
}

#[test]
fn p1_orchestrator_skips_duplicate_names_deterministically() {
    // Some communities ship multiple skills with the same
    // frontmatter `name` in different directories. The
    // orchestrator's `seen_names` set keeps the first one only —
    // first-write-wins, never random. We verify that by writing
    // two SKILL.md files with the same `name` field and
    // confirming exactly one command is registered.
    let tmp = tempfile::tempdir().expect("tempdir");

    let dir_a = tmp.path().join("alpha");
    let dir_b = tmp.path().join("beta");
    fs::create_dir_all(&dir_a).unwrap();
    fs::create_dir_all(&dir_b).unwrap();
    fs::write(
        dir_a.join("SKILL.md"),
        "---\nname: shared\ndescription: First instance\n---\nfirst body",
    )
    .unwrap();
    fs::write(
        dir_b.join("SKILL.md"),
        "---\nname: shared\ndescription: Second instance\n---\nsecond body",
    )
    .unwrap();

    let mut orch = SkillOrchestrator::new(tmp.path());
    orch.scan_skill_commands();

    let matches: Vec<_> = orch
        .get_commands()
        .iter()
        .filter(|(_k, v)| v.name == "shared")
        .collect();
    assert_eq!(
        matches.len(),
        1,
        "exactly one 'shared' command expected; got {}",
        matches.len()
    );
    // First-write-wins → the first scanner walked `alpha/` before
    // `beta/` (alphabetical).
    assert_eq!(matches[0].1.description, "First instance");
}
