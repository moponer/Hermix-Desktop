//! Skills management tools: list, view, and manage skills

use async_trait::async_trait;
use indexmap::IndexMap;
use serde_json::{json, Value};

use hermix_core::{
    tool_schema, traits::RiskLevel, JsonSchema, SkillProvider, ToolError, ToolHandler, ToolSchema,
};

use std::sync::Arc;

use crate::ToolRegistry;

// ---------------------------------------------------------------------------
// Registration (P1.3 migration: skills tools move out of register_builtins.rs)
// ---------------------------------------------------------------------------

/// Register the three skill tools: `skills_list`, `skill_view`,
/// `skill_manage`. The `skills_list` + `skill_view` tools are read-only
/// (Safe); `skill_manage` writes to disk and is Caution. All three
/// share the same `skill_provider` so writes done through
/// `skill_manage` are visible to subsequent `skills_list` /
/// `skill_view` calls inside the same session.
pub fn register(registry: &ToolRegistry, skill_provider: Arc<dyn SkillProvider>) {
    registry.register_with_risk(
        "skills_list",
        "skills",
        SkillsListHandler::new(skill_provider.clone()).schema(),
        Arc::new(SkillsListHandler::new(skill_provider.clone())),
        Arc::new(|| true),
        vec![],
        true,
        "List all available skills with their metadata.",
        "📚",
        None,
        RiskLevel::Safe,
        /* override_existing = */ false,
    );
    registry.register_with_risk(
        "skill_view",
        "skills",
        SkillViewHandler::new(skill_provider.clone()).schema(),
        Arc::new(SkillViewHandler::new(skill_provider.clone())),
        Arc::new(|| true),
        vec![],
        true,
        "View the full content of a skill by name.",
        "📖",
        None,
        RiskLevel::Safe,
        /* override_existing = */ false,
    );
    registry.register_with_risk(
        "skill_manage",
        "skills",
        SkillManageHandler::new(skill_provider.clone()).schema(),
        Arc::new(SkillManageHandler::new(skill_provider)),
        Arc::new(|| true),
        vec![],
        true,
        "Create, update, delete, auto-extract, or self-improve skills. See schema for the full action set (auto_create / auto_extract_from_conversation enforce quality gates).",
        "⚙️",
        None,
        RiskLevel::Caution,
        /* override_existing = */ false,
    );
}

// ---------------------------------------------------------------------------
// SkillsListHandler
// ---------------------------------------------------------------------------

/// Tool for listing all available skills.
pub struct SkillsListHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillsListHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillsListHandler {
    async fn execute(&self, _params: Value) -> Result<String, ToolError> {
        let skills = self
            .provider
            .list_skills()
            .await
            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

        let result: Vec<Value> = skills
            .iter()
            .map(|s| {
                json!({
                    "name": s.name,
                    "category": s.category,
                    "description": s.description,
                })
            })
            .collect();

        Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| "[]".to_string()))
    }

    fn schema(&self) -> ToolSchema {
        tool_schema(
            "skills_list",
            "List all available skills with their metadata.",
            JsonSchema::new("object"),
        )
    }
}

// ---------------------------------------------------------------------------
// SkillViewHandler
// ---------------------------------------------------------------------------

/// Tool for viewing a specific skill's content.
pub struct SkillViewHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillViewHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillViewHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

        match self.provider.get_skill(name).await {
            Ok(Some(skill)) => Ok(skill.content.clone()),
            Ok(None) => Err(ToolError::NotFound(format!("Skill '{}' not found", name))),
            Err(e) => Err(ToolError::ExecutionFailed(e.to_string())),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Name of the skill to view"
            }),
        );

        tool_schema(
            "skill_view",
            "View the full content of a skill by name.",
            JsonSchema::object(props, vec!["name".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// SkillManageHandler
// ---------------------------------------------------------------------------

/// Tool for creating, updating, and deleting skills.
pub struct SkillManageHandler {
    provider: Arc<dyn SkillProvider>,
}

impl SkillManageHandler {
    pub fn new(provider: Arc<dyn SkillProvider>) -> Self {
        Self { provider }
    }
}

#[async_trait]
impl ToolHandler for SkillManageHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ToolError::InvalidParams("Missing 'action' parameter".into()))?;

        match action {
            "create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;
                let category = params.get("category").and_then(|v| v.as_str());

                self.provider.create_skill(name, content, category)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' created successfully", name))
            }
            "update" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let content = params.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'content' parameter".into()))?;

                self.provider.update_skill(name, content)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' updated successfully", name))
            }
            "delete" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;

                self.provider.delete_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' deleted successfully", name))
            }
            "auto_create" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let summary = params.get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Auto-generated from completed task.");
                let steps = params.get("steps")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                //quality gate. Reject skills that are obviously
                // noise — too short, all whitespace, or the
                // placeholder default. Without this, an LLM that gets
                // distracted mid-extraction writes empty skills that
                // pollute `~/.hermix/skills/` and break slash-command
                // resolution.
                let trimmed_name = name.trim();
                let trimmed_summary = summary.trim();
                if trimmed_name.is_empty()
                    || trimmed_name.len() < 3
                    || trimmed_name.len() > 80
                {
                    return Err(ToolError::InvalidParams(format!(
                        "Skill name must be 3–80 chars (got {} chars)",
                        trimmed_name.len()
                    )));
                }
                if trimmed_summary.is_empty()
                    || trimmed_summary == "Auto-generated from completed task."
                {
                    return Err(ToolError::InvalidParams(
                        "Skill summary is empty or placeholder — refusing to write a low-quality skill".into(),
                    ));
                }
                if trimmed_summary.len() < 10 {
                    return Err(ToolError::InvalidParams(format!(
                        "Skill summary too short ({} chars, min 10)",
                        trimmed_summary.len()
                    )));
                }
                if steps.is_empty() {
                    return Err(ToolError::InvalidParams(
                        "Skill must have at least one step — refusing to write a skill with no actionable content".into(),
                    ));
                }

                //frontmatter. Without `---\nname: ...\n---`
                // frontmatter, `SkillOrchestrator::parse_frontmatter`
                // still works (it falls back to directory name) but
                // the skill becomes ambiguous when the LLM later
                // invokes `/<name>` and there's a sibling file with
                // a colliding dirname. Emitting frontmatter makes
                // the skill name authoritative and lets the
                // orchestrator skip the dirname fallback.
                let description: String = trimmed_summary
                    .chars()
                    .take(120)
                    .collect();
                let mut content = format!(
                    "---\nname: {}\ndescription: {}\ncategory: auto-generated\n---\n\n# {}\n\n## Summary\n{}\n\n## Steps\n",
                    trimmed_name, description, trimmed_name, trimmed_summary,
                );
                for (idx, s) in steps.iter().enumerate() {
                    content.push_str(&format!("{}. {}\n", idx + 1, s));
                }

                //dedup. If a skill with the same name
                // already exists, route to `update_skill` instead
                // of creating a duplicate. Without this, every
                // `skill_creation_nudge_interval` ticks the LLM
                // races to create `commit-msg-formatting` /
                // `commit-msg-style` / `commit-msg-convention`
                // until the disk is full.
                let existed = self.provider.get_skill(trimmed_name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .is_some();
                if existed {
                    self.provider.update_skill(trimmed_name, &content)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                    Ok(format!(
                        "Skill '{}' already existed — auto-updated with new summary/steps",
                        trimmed_name
                    ))
                } else {
                    self.provider.create_skill(trimmed_name, &content, Some("auto-generated"))
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                    Ok(format!("Skill '{}' auto-created", trimmed_name))
                }
            }
            "auto_extract_from_conversation" => {
                //deterministic extraction path. The
                // `SKILL_REVIEW_PROMPT` background-review loop is
                // stochastic — the LLM may decide to write
                // "Nothing to save." even when a clear reusable
                // pattern emerged. This action takes the
                // already-summarized conversation and writes a
                // skill *without* a second LLM round-trip,
                // guaranteeing the learning-loop closure rate.
                // Args:
                // name:       short slug, e.g. "commit-msg-style"
                // category:   "auto-extracted" (default)
                // summary:    one-paragraph "what was learned"
                // steps:      ordered list of "do X then Y then Z"
                // rationale:  why this is worth saving (optional)
                // Note: `name` validation mirrors `auto_create`'s
                // quality gate above. If validation fails here,
                // we surface the same error to the caller so the
                // background review task can retry with a better
                // name instead of silently writing a degraded
                // skill.
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let trimmed_name = name.trim();
                if trimmed_name.len() < 3 || trimmed_name.len() > 80 {
                    return Err(ToolError::InvalidParams(format!(
                        "Skill name must be 3–80 chars (got {})",
                        trimmed_name.len()
                    )));
                }
                let category = params.get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("auto-extracted");
                let summary = params.get("summary")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let steps: Vec<String> = params.get("steps")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .map(String::from)
                            .collect()
                    })
                    .unwrap_or_default();
                let rationale = params.get("rationale").and_then(|v| v.as_str());

                if summary.trim().is_empty() || summary.len() < 10 {
                    return Err(ToolError::InvalidParams(
                        "summary must be a non-empty sentence (>10 chars)".into(),
                    ));
                }
                if steps.is_empty() {
                    return Err(ToolError::InvalidParams(
                        "steps must list at least one concrete action".into(),
                    ));
                }

                let description: String = summary.chars().take(120).collect();
                let mut content = format!(
                    "---\nname: {}\ndescription: {}\ncategory: {}\n---\n\n# {}\n\n## Summary\n{}\n\n## Steps\n",
                    trimmed_name, description, category, trimmed_name, summary,
                );
                for (idx, s) in steps.iter().enumerate() {
                    content.push_str(&format!("{}. {}\n", idx + 1, s));
                }
                if let Some(r) = rationale {
                    if !r.trim().is_empty() {
                        content.push_str(&format!("\n## Why this works\n{}\n", r.trim()));
                    }
                }

                let existed = self.provider.get_skill(trimmed_name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .is_some();
                if existed {
                    self.provider.update_skill(trimmed_name, &content)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                    Ok(format!("Skill '{}' auto-extracted (updated existing)", trimmed_name))
                } else {
                    self.provider.create_skill(trimmed_name, &content, Some(category))
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                    Ok(format!("Skill '{}' auto-extracted", trimmed_name))
                }
            }
            "self_improve" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let feedback = params.get("feedback")
                    .and_then(|v| v.as_str())
                    .unwrap_or("No feedback provided.");
                let existing = self.provider.get_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .ok_or_else(|| ToolError::NotFound(format!("Skill '{}' not found", name)))?;
                let improved = format!("{}\n\n## Improvement Feedback\n{}\n", existing.content, feedback);
                self.provider.update_skill(name, &improved)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Skill '{}' improved", name))
            }
            "sync" => Ok("Skill sync request accepted (provider-specific hub sync path).".to_string()),
            "import_from_url" => {
                //: ingest a community SKILL.md from
                // a URL, parse the agentskills.io open-standard
                // frontmatter, write it to ~/.hermix/skills/<slug>/SKILL.md.
                // Why HTTP fetch and not just `download_skill` from
                // hermix-skills::hub? Because:
                // 1. The URL might be a raw GitHub gist/blob, not a
                // hub API endpoint.
                // 2. It also lets the LLM pull a single skill from
                // `agentskills.io/<slug>/SKILL.md` without
                // knowing the hub protocol.
                // We deliberately do NOT shell out to `curl` or
                // `Invoke-WebRequest` — `reqwest` is already a
                // workspace dep and handles proxies / TLS / retries
                // consistently.
                let url = params.get("url")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'url' parameter".into()))?
                    .trim();
                if url.is_empty() {
                    return Err(ToolError::InvalidParams("'url' must not be empty".into()));
                }
                // Defensive URL guard. agentskills.io + github raw
                // are the canonical sources; everything else is
                // rejected so a compromised skill can't exfiltrate
                // data through a side channel.
                if !(url.starts_with("https://agentskills.io/")
                    || url.starts_with("https://raw.githubusercontent.com/")
                    || url.starts_with("https://gist.githubusercontent.com/"))
                {
                    return Err(ToolError::InvalidParams(format!(
                        "P2-8 import_from_url: only agentskills.io / raw.githubusercontent.com / \
                         gist.githubusercontent.com are supported (got {})",
                        url
                    )));
                }

                let response = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(15))
                    .build()
                    .map_err(|e| ToolError::ExecutionFailed(format!("http client build: {e}")))?
                    .get(url)
                    .send()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("fetch failed: {e}")))?;
                if !response.status().is_success() {
                    return Err(ToolError::ExecutionFailed(format!(
                        "fetch returned status {}",
                        response.status()
                    )));
                }
                let body = response
                    .text()
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(format!("fetch body: {e}")))?;

                // Parse frontmatter via the local skill_utils helper so the import
                // path reuses the same parsing the rest of the tool
                // stack uses (HashMap-based, serde_yaml-backed).
                let (fm, _body) = crate::tools::skill_utils::parse_frontmatter(&body);
                let name = fm
                    .get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| {
                        ToolError::InvalidParams(
                            "P2-8 import_from_url: SKILL.md is missing required `name` frontmatter".into(),
                        )
                    })?
                    .to_string();
                let desc = fm
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let category = fm
                    .get("category")
                    .and_then(|v| v.as_str())
                    .unwrap_or("agentskills-io")
                    .to_string();

                // Dedup by name: if the skill already exists locally
                // we update it (matching the auto_extract policy).
                let existed = self.provider.get_skill(&name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .is_some();
                if existed {
                    self.provider.update_skill(&name, &body)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                } else {
                    self.provider
                        .create_skill(&name, &body, Some(&category))
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                }
                Ok(format!(
                    "P2-8 imported '{}' from {} (description: {}, category: {}{})",
                    name,
                    url,
                    desc.chars().take(60).collect::<String>(),
                    category,
                    if existed { ", updated existing" } else { ", created" },
                ))
            }
            "install_builtins" => {
                let builtins = [
                    "planning","debugging","refactoring","testing","docs","git","review","web-research",
                    "terminal","file-edit","security","performance","api-design","db-migrations",
                    "incident-response","release","prompting","agent-orchestration","mcp","gateway",
                    "voice-mode","cron","memory","session-search","tool-authoring","skill-authoring"
                ];
                let mut created = 0usize;
                for name in builtins {
                    let exists = self.provider.get_skill(name)
                        .await
                        .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                        .is_some();
                    if !exists {
                        let content = format!("# {}\n\n1. Understand\n2. Execute\n3. Verify\n", name);
                        self.provider.create_skill(name, &content, Some("builtin"))
                            .await
                            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                        created += 1;
                    }
                }
                Ok(format!("Installed {} built-in skills", created))
            }
            // ── offline .hermes-skill file format ──
            "export_to_file" => {
                let name = params.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'name' parameter".into()))?;
                let file_path = params.get("file_path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'file_path' parameter".into()))?;

                let meta = self.provider.get_skill(name)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?
                    .ok_or_else(|| ToolError::ExecutionFailed(format!(
                        "Skill '{}' not found", name
                    )))?;

                // .hermes-skill v1 bundle format: JSON with frontmatter fields.
                let bundle = json!({
                    "hermix_skill": "1",
                    "name": meta.name,
                    "description": meta.description,
                    "category": meta.category,
                    "content": meta.content,
                });
                let bundle_str = serde_json::to_string_pretty(&bundle)
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                std::fs::write(file_path, &bundle_str)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to write .hermes-skill file: {}", e)))?;
                Ok(format!("Exported skill '{}' to {}", name, file_path))
            }
            "import_from_file" => {
                let file_path = params.get("file_path")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::InvalidParams("Missing 'file_path' parameter".into()))?;

                let raw = std::fs::read_to_string(file_path)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Failed to read .hermes-skill file: {}", e)))?;
                let bundle: Value = serde_json::from_str(&raw)
                    .map_err(|e| ToolError::ExecutionFailed(format!("Invalid .hermes-skill JSON: {}", e)))?;

                let version = bundle.get("hermix_skill")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::ExecutionFailed(
                        "Missing 'hermix_skill' version field — not a valid .hermes-skill file".into()
                    ))?;
                if version != "1" {
                    return Err(ToolError::ExecutionFailed(format!(
                        "Unsupported .hermes-skill version '{}' — expected '1'", version
                    )));
                }

                let name = bundle.get("name")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::ExecutionFailed("Missing 'name' in bundle".into()))?;
                let content = bundle.get("content")
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| ToolError::ExecutionFailed("Missing 'content' in bundle".into()))?;
                let category = bundle.get("category").and_then(|v| v.as_str());

                self.provider.create_skill(name, content, category)
                    .await
                    .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;
                Ok(format!("Imported skill '{}' from {}", name, file_path))
            }
            other => Err(ToolError::InvalidParams(format!("Unknown action: '{}'. Use create/update/delete/auto_create/self_improve/sync/install_builtins/export_to_file/import_from_file.", other))),
        }
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert("action".into(), json!({
            "type": "string",
            "description": "Action to perform",
            "enum": [
                "create",
                "update",
                "delete",
                "auto_create",
                "auto_extract_from_conversation",
                "self_improve",
                "sync",
                "install_builtins",
                "export_to_file",
                "import_from_file"
            ]
        }));
        props.insert(
            "name".into(),
            json!({
                "type": "string",
                "description": "Name of the skill (3–80 chars for auto_* actions)"
            }),
        );
        props.insert(
            "content".into(),
            json!({
                "type": "string",
                "description": "Skill content (for create/update)"
            }),
        );
        props.insert(
            "category".into(),
            json!({
                "type": "string",
                "description": "Skill category (for create/auto_extract_from_conversation). Defaults to 'auto-extracted' for auto_extract_from_conversation."
            }),
        );
        props.insert(
            "summary".into(),
            json!({
                "type": "string",
                "description": "Task summary used by auto_create and auto_extract_from_conversation (>10 chars; rejected if empty or placeholder)."
            }),
        );
        props.insert(
            "steps".into(),
            json!({
                "type": "array",
                "items": {"type": "string"},
                "description": "Step list for auto_create / auto_extract_from_conversation (must be non-empty)."
            }),
        );
        props.insert(
            "feedback".into(),
            json!({
                "type": "string",
                "description": "Feedback used by self_improve"
            }),
        );
        props.insert(
            "rationale".into(),
            json!({
                "type": "string",
                "description": "Optional 'why this works' paragraph for auto_extract_from_conversation"
            }),
        );
        // .hermes-skill offline file share format.
        props.insert(
            "file_path".into(),
            json!({
                "type": "string",
                "description": "File path for export_to_file / import_from_file actions (must end with .hermes-skill)"
            }),
        );
        props.insert(
            "source_url".into(),
            json!({
                "type": "string",
                "description": "URL for import_from_url action (agentskills.io or raw.githubusercontent.com)"
            }),
        );

        tool_schema(
            "skill_manage",
            "Create, update, delete, auto-extract, self-improve, sync, install builtins, or export/import .hermes-skill files. auto_create / auto_extract_from_conversation enforce name (3–80 chars), summary (>10 chars, non-placeholder), and steps (≥1) — sub-quality calls return an error instead of writing noise. Both auto actions write SKILL.md frontmatter so the orchestrator picks them up cleanly, and dedup against an existing skill of the same name (update instead of duplicate-create). export_to_file writes a .hermes-skill v1 JSON bundle; import_from_file reads it.",
            JsonSchema::object(props, vec!["action".into(), "name".into()]),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermix_core::{AgentError, Skill, SkillMeta};
    use std::sync::Mutex;

    /// Records every provider call so tests can assert which path
    /// was taken (create vs update for dedup, content shape for
    /// frontmatter, etc.).
    #[derive(Default)]
    struct CallLog {
        creates: Vec<(String, String, Option<String>)>,
        updates: Vec<(String, String)>,
    }

    struct MockSkillProvider {
        /// Skills that `get_skill` reports as existing — drives
        /// the auto_create dedup path.
        existing: std::collections::HashSet<String>,
        log: Mutex<CallLog>,
    }

    impl MockSkillProvider {
        fn new(existing: &[&str]) -> Self {
            Self {
                existing: existing.iter().map(|s| s.to_string()).collect(),
                log: Mutex::new(CallLog::default()),
            }
        }
        fn snapshot(&self) -> CallLog {
            // The test inspects a snapshot copy so the borrow
            // checker is happy.
            let g = self.log.lock().unwrap();
            CallLog {
                creates: g.creates.clone(),
                updates: g.updates.clone(),
            }
        }
    }

    #[async_trait]
    impl SkillProvider for MockSkillProvider {
        async fn create_skill(
            &self,
            name: &str,
            content: &str,
            category: Option<&str>,
        ) -> Result<Skill, AgentError> {
            self.log
                .lock()
                .unwrap()
                .creates
                .push((name.into(), content.into(), category.map(String::from)));
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: category.map(String::from),
                description: None,
            })
        }
        async fn get_skill(&self, name: &str) -> Result<Option<Skill>, AgentError> {
            if self.existing.contains(name) {
                Ok(Some(Skill {
                    name: name.into(),
                    content: "existing content".into(),
                    category: Some("auto-generated".into()),
                    description: None,
                }))
            } else {
                Ok(None)
            }
        }
        async fn list_skills(&self) -> Result<Vec<SkillMeta>, AgentError> {
            Ok(vec![SkillMeta {
                name: "test".into(),
                category: None,
                description: None,
            }])
        }
        async fn update_skill(&self, name: &str, content: &str) -> Result<Skill, AgentError> {
            self.log
                .lock()
                .unwrap()
                .updates
                .push((name.into(), content.into()));
            Ok(Skill {
                name: name.into(),
                content: content.into(),
                category: None,
                description: None,
            })
        }
        async fn delete_skill(&self, _name: &str) -> Result<(), AgentError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_skills_list() {
        let handler = SkillsListHandler::new(Arc::new(MockSkillProvider::new(&[])));
        let result = handler.execute(json!({})).await.unwrap();
        assert!(result.contains("test"));
    }

    #[tokio::test]
    async fn test_skill_view() {
        let handler = SkillViewHandler::new(Arc::new(MockSkillProvider::new(&["test"])));
        let result = handler.execute(json!({"name": "test"})).await.unwrap();
        assert_eq!(result, "existing content");
    }

    #[tokio::test]
    async fn test_skill_view_missing_returns_not_found() {
        let handler = SkillViewHandler::new(Arc::new(MockSkillProvider::new(&[])));
        let result = handler.execute(json!({"name": "nope"})).await;
        assert!(matches!(result, Err(ToolError::NotFound(_))));
    }

    #[tokio::test]
    async fn test_skill_manage_create() {
        let handler = SkillManageHandler::new(Arc::new(MockSkillProvider::new(&[])));
        let result = handler
            .execute(json!({"action": "create", "name": "new_skill", "content": "hello"}))
            .await
            .unwrap();
        assert!(result.contains("created"));
    }

    // -----------------------------------------------------------------
    //auto_create quality gate
    // -----------------------------------------------------------------

    fn valid_auto_create_payload() -> Value {
        json!({
            "action": "auto_create",
            "name": "commit-msg-style",
            "summary": "Format commit messages as Conventional Commits with a 72-char subject line.",
            "steps": [
                "Read git log --oneline -5 to match house style",
                "Run git diff --staged to inspect pending changes",
                "Compose subject ≤ 72 chars, imperative mood",
                "Add body wrapped at 72 cols if non-trivial",
            ],
        })
    }

    #[tokio::test]
    async fn auto_create_writes_frontmatter_and_creates_when_absent() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider.clone());
        let result = handler.execute(valid_auto_create_payload()).await.unwrap();
        assert!(result.contains("auto-created"), "got: {result}");
        let log = provider.snapshot();
        assert_eq!(log.creates.len(), 1, "should create, not update");
        assert_eq!(log.updates.len(), 0);
        let (name, content, category) = &log.creates[0];
        assert_eq!(name, "commit-msg-style");
        assert_eq!(category.as_deref(), Some("auto-generated"));
        // Frontmatter must be present so SkillOrchestrator picks
        // the skill up cleanly even if the dirname diverges.
        assert!(content.starts_with("---\n"), "missing YAML frontmatter opener");
        assert!(content.contains("name: commit-msg-style"));
        assert!(content.contains("category: auto-generated"));
        assert!(content.contains("## Steps\n"));
        assert!(content.contains("1. Read git log --oneline -5"));
    }

    #[tokio::test]
    async fn auto_create_dedupes_to_update_when_skill_exists() {
        let provider = Arc::new(MockSkillProvider::new(&["commit-msg-style"]));
        let handler = SkillManageHandler::new(provider.clone());
        let result = handler.execute(valid_auto_create_payload()).await.unwrap();
        assert!(result.contains("auto-updated"), "got: {result}");
        let log = provider.snapshot();
        assert_eq!(log.creates.len(), 0, "must NOT create duplicate");
        assert_eq!(log.updates.len(), 1, "must update the existing skill");
        assert_eq!(log.updates[0].0, "commit-msg-style");
    }

    #[tokio::test]
    async fn auto_create_rejects_short_name() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_auto_create_payload();
        payload["name"] = json!("ab");
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("3–80 chars"), "got: {err}");
    }

    #[tokio::test]
    async fn auto_create_rejects_long_name() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_auto_create_payload();
        payload["name"] = json!("a".repeat(81).as_str());
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("3–80 chars"));
    }

    #[tokio::test]
    async fn auto_create_rejects_placeholder_summary() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_auto_create_payload();
        payload["summary"] = json!("Auto-generated from completed task.");
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("placeholder"));
    }

    #[tokio::test]
    async fn auto_create_rejects_short_summary() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_auto_create_payload();
        payload["summary"] = json!("too short");
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("too short"));
    }

    #[tokio::test]
    async fn auto_create_rejects_empty_steps() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_auto_create_payload();
        payload["steps"] = json!([]);
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("at least one step"));
    }

    // -----------------------------------------------------------------
    //auto_extract_from_conversation
    // -----------------------------------------------------------------

    fn valid_extract_payload() -> Value {
        json!({
            "action": "auto_extract_from_conversation",
            "name": "hermetic-build-on-windows",
            "summary": "On Windows, set LIB/INCLUDE/PATH and use rust-lld before any cargo build to dodge the Git-bundled link.exe shadow and the missing kernel32.lib LNK1181 error.",
            "steps": [
                "Detect Git Bash and prepend MSVC bin + Windows Kits bin to PATH (after stripping Git's link.exe).",
                "Set LIB and INCLUDE for Hostx64/x64 + ucrt/um/shared.",
                "Pin rust-lld via .cargo/config.toml link = .../link.exe absolute path.",
                "Wrap cargo / pnpm in a bat if invoking from cmd.exe.",
            ],
            "rationale": "Three prior sessions lost hours to this; recording it once means future agents skip the diagnosis.",
        })
    }

    #[tokio::test]
    async fn auto_extract_writes_frontmatter_and_creates_when_absent() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider.clone());
        let result = handler.execute(valid_extract_payload()).await.unwrap();
        assert!(result.contains("auto-extracted"), "got: {result}");
        let log = provider.snapshot();
        assert_eq!(log.creates.len(), 1);
        assert_eq!(log.updates.len(), 0);
        let (name, content, category) = &log.creates[0];
        assert_eq!(name, "hermetic-build-on-windows");
        assert_eq!(category.as_deref(), Some("auto-extracted"));
        assert!(content.contains("## Why this works\n"));
        assert!(content.contains("Three prior sessions lost hours"));
    }

    #[tokio::test]
    async fn auto_extract_dedupes_when_skill_exists() {
        let provider = Arc::new(MockSkillProvider::new(&["hermetic-build-on-windows"]));
        let handler = SkillManageHandler::new(provider.clone());
        let result = handler.execute(valid_extract_payload()).await.unwrap();
        assert!(result.contains("updated existing"), "got: {result}");
        let log = provider.snapshot();
        assert_eq!(log.creates.len(), 0);
        assert_eq!(log.updates.len(), 1);
    }

    #[tokio::test]
    async fn auto_extract_rejects_empty_summary() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_extract_payload();
        payload["summary"] = json!("");
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("non-empty sentence"));
    }

    #[tokio::test]
    async fn auto_extract_rejects_empty_steps() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider);
        let mut payload = valid_extract_payload();
        payload["steps"] = json!([]);
        let err = handler.execute(payload).await.unwrap_err();
        assert!(format!("{err}").contains("at least one concrete action"));
    }

    #[tokio::test]
    async fn auto_extract_truncates_long_description_to_120_chars() {
        let provider = Arc::new(MockSkillProvider::new(&[]));
        let handler = SkillManageHandler::new(provider.clone());
        let mut payload = valid_extract_payload();
        payload["summary"] = json!("x".repeat(500).as_str());
        let result = handler.execute(payload).await.unwrap();
        let log = provider.snapshot();
        let (_, content, _) = &log.creates[0];
        // description in frontmatter must be ≤ 120 chars
        let desc_line = content
            .lines()
            .find(|l| l.starts_with("description:"))
            .expect("description line");
        // "description: " prefix is 14 chars
        assert!(
            desc_line.len() <= 14 + 120,
            "description too long: {desc_line:?}"
        );
        assert!(result.contains("auto-extracted"));
    }
}
