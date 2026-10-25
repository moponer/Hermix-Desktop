//! 成员角色文件解析（总管方案 D2/P1）。
//!
//! 角色文件 = YAML frontmatter + 正文 system prompt，放
//! `~/.hermix/agents/*.md`：
//!
//! ```markdown
//! ---
//! name: reviewer
//! description: 对照验收标准评审交付物。
//! toolset: read_only          # read_only | full | 逗号分隔工具名单
//! model: inherit              # inherit/留空 = 跟随宿主配置
//! worker: internal            # internal | external:<driver> | browser
//! resources: {mem_gb: 2, cpu_weight: 3, scratch_cap_gb: 5}
//! version: 1
//! ---
//! （该成员的 system prompt 正文）
//! ```
//!
//! 借鉴 Claude Code subagents 的文件格式（只借格式，不借其瞬时调用
//! 语义——成员是长任务执行单元，不是一次工具调用）。未知 frontmatter
//! 字段放行（向前兼容）；`name` 缺失或为空视为坏文件，加载时跳过并
//! 记错误，不让一个坏文件拖垮整个角色库。
//!
//! `resources.cpu_weight` 直接采用 Windows Job Object 的 1..=9 标度
//! （默认 5），与 `hermix_core::win_job::JobLimits` 同一刻度，不引入
//! 第二套单位换算。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 角色文件默认目录：`~/.hermix/agents`。
pub fn default_roles_dir() -> PathBuf {
    hermix_config::paths::hermix_home().join("agents")
}

/// 成员的 worker 形态（frontmatter `worker` 字段）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum MemberWorker {
    /// internal 成员会话（独立持久 AgentLoop 会话）。
    #[default]
    Internal,
    /// 外部 CLI 客户端，`external:codex` 中的 driver 名。
    External(String),
    /// 现有 headless Edge + CDP。
    Browser,
}

/// 资源档（Job Object 同刻度）。任一字段可缺省，spawn 时按
/// `JobLimits::worker_defaults` 兜底。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberResources {
    /// 提交内存上限（GiB）。
    #[serde(default)]
    pub mem_gb: Option<u32>,
    /// CPU 调度权重 1..=9（Job Object 标度，默认 5）。
    #[serde(default)]
    pub cpu_weight: Option<u32>,
    /// scratch 目录配额（GiB）。
    #[serde(default)]
    pub scratch_cap_gb: Option<u32>,
}

impl MemberResources {
    /// `agent_role_upsert` 新建角色时的缺省资源档（量级对齐
    /// `JobLimits::worker_defaults`）。
    pub fn upsert_default() -> Self {
        Self {
            mem_gb: Some(4),
            cpu_weight: Some(5),
            scratch_cap_gb: Some(5),
        }
    }
}

/// 一个成员角色（文件解析结果）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemberRole {
    pub name: String,
    pub description: String,
    /// `read_only` / `full` / 逗号分隔工具名单；`None` = 继承全部。
    #[serde(default)]
    pub toolset: Option<String>,
    /// `inherit`/空 = `None`（跟随宿主配置）。
    #[serde(default)]
    pub model: Option<String>,
    pub worker: MemberWorker,
    #[serde(default)]
    pub resources: MemberResources,
    pub version: u32,
    /// frontmatter 之后的正文，成员会话的 system prompt 主体。
    pub system_prompt: String,
    /// 来源文件（内置种子落盘后同样有真实路径）。
    #[serde(skip)]
    pub source_path: PathBuf,
}

/// 角色库加载结果：好的角色 + 坏文件清单（不让一个坏文件拖垮全部）。
#[derive(Debug, Clone, Default)]
pub struct RoleLibrary {
    /// 按 `name` 排序。
    pub roles: Vec<MemberRole>,
    /// (文件, 原因)。
    pub errors: Vec<(PathBuf, String)>,
}

impl RoleLibrary {
    pub fn find(&self, name: &str) -> Option<&MemberRole> {
        let name = name.trim();
        self.roles.iter().find(|r| r.name == name)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RoleParseError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frontmatter 缺失（文件必须以 `---` 行开头）")]
    MissingFrontmatter,
    #[error("frontmatter YAML 解析失败: {0}")]
    Yaml(String),
    #[error("字段无效: {0}")]
    InvalidField(String),
}

/// YAML frontmatter 中间结构。未知字段放行（向前兼容）。
#[derive(Debug, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    toolset: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    worker: String,
    #[serde(default)]
    resources: MemberResources,
    #[serde(default)]
    version: Option<u32>,
}

/// 解析一份角色文件内容（不含读盘）。
pub fn parse_role_content(path: &Path, content: &str) -> Result<MemberRole, RoleParseError> {
    let rest = content
        .strip_prefix("---\n")
        .or_else(|| content.strip_prefix("---\r\n"))
        .ok_or(RoleParseError::MissingFrontmatter)?;
    // frontmatter 在下一个整行 `---` 处结束；正文是其后所有内容。
    let (fm_src, body) = match rest.find("\n---") {
        Some(idx) => {
            let after = &rest[idx + 1..]; // 从 `\n` 后的 `---` 起
            let body = after
                .strip_prefix("---\n")
                .or_else(|| after.strip_prefix("---\r\n"))
                .unwrap_or_else(|| after.trim_start_matches("---"));
            (&rest[..idx], body)
        }
        None => return Err(RoleParseError::MissingFrontmatter),
    };

    let fm: Frontmatter = serde_yaml::from_str(fm_src)
        .map_err(|e| RoleParseError::Yaml(e.to_string()))?;

    let name = fm
        .name
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .ok_or_else(|| RoleParseError::InvalidField("name 缺失或为空".to_string()))?;

    let model = fm
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && m != "inherit");

    let toolset = fm
        .toolset
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());

    let worker = if fm.worker.trim().is_empty() {
        MemberWorker::Internal
    } else {
        match fm.worker.trim() {
            "internal" => MemberWorker::Internal,
            "browser" => MemberWorker::Browser,
            other => match other.strip_prefix("external:") {
                Some(driver) if !driver.trim().is_empty() => {
                    MemberWorker::External(driver.trim().to_string())
                }
                _ => {
                    return Err(RoleParseError::InvalidField(format!(
                        "worker `{other}` 不认识（internal | external:<driver> | browser）"
                    )))
                }
            },
        }
    };

    if let Some(w) = fm.resources.cpu_weight {
        if !(1..=9).contains(&w) {
            return Err(RoleParseError::InvalidField(format!(
                "resources.cpu_weight {w} 超出 Job Object 标度 1..=9"
            )));
        }
    }

    Ok(MemberRole {
        name,
        description: fm.description,
        toolset,
        model,
        worker,
        resources: fm.resources,
        version: fm.version.unwrap_or(1),
        system_prompt: body.trim().to_string(),
        source_path: path.to_path_buf(),
    })
}

/// 加载目录下全部 `*.md` 角色文件。文件名排序保证确定性；同名角色
/// （后加载者）记入 errors 而不是静默覆盖。
pub fn load_role_library(dir: &Path) -> RoleLibrary {
    let mut library = RoleLibrary::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return library;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "md"))
        .collect();
    files.sort();

    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    for path in files {
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                library.errors.push((path.clone(), e.to_string()));
                continue;
            }
        };
        match parse_role_content(&path, &content) {
            Ok(role) => {
                if seen.insert(role.name.clone(), ()).is_some() {
                    library.errors.push((
                        path.clone(),
                        format!("角色名 `{}` 与已加载文件重复", role.name),
                    ));
                    continue;
                }
                library.roles.push(role);
            }
            Err(e) => library.errors.push((path.clone(), e.to_string())),
        }
    }
    library
}

/// 内置角色种子：`(文件名, 内容)`。首跑落盘到角色目录；**已存在不覆盖**
/// （用户/阿黎可能已改过，覆盖 = 丢用户数据）。
pub fn builtin_role_sources() -> &'static [(&'static str, &'static str)] {
    &[
        ("reviewer.md", include_str!("builtin/reviewer.md")),
        ("coder.md", include_str!("builtin/coder.md")),
        ("registrar.md", include_str!("builtin/registrar.md")),
    ]
}

/// 确保内置角色存在（目录创建 + 缺失文件落盘）。返回实际写入的文件数。
pub fn ensure_builtin_roles(dir: &Path) -> std::io::Result<usize> {
    std::fs::create_dir_all(dir)?;
    let mut written = 0;
    for (file_name, content) in builtin_role_sources() {
        let path = dir.join(file_name);
        if !path.exists() {
            std::fs::write(&path, content)?;
            written += 1;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "---\nname: reviewer\ndescription: 评审交付物\ntoolset: read_only\nmodel: inherit\nworker: internal\nresources: {mem_gb: 2, cpu_weight: 3, scratch_cap_gb: 5}\nversion: 7\n---\n你是评审员。\n只做核验。\n";

    #[test]
    fn parses_all_fields() {
        let role = parse_role_content(Path::new("reviewer.md"), FULL).expect("parse");
        assert_eq!(role.name, "reviewer");
        assert_eq!(role.description, "评审交付物");
        assert_eq!(role.toolset.as_deref(), Some("read_only"));
        assert_eq!(role.model, None, "model: inherit 应归一为 None");
        assert_eq!(role.worker, MemberWorker::Internal);
        assert_eq!(role.resources.mem_gb, Some(2));
        assert_eq!(role.resources.cpu_weight, Some(3));
        assert_eq!(role.version, 7);
        assert!(role.system_prompt.starts_with("你是评审员。"));
        assert!(role.system_prompt.contains("只做核验。"));
    }

    #[test]
    fn defaults_when_optional_fields_missing() {
        let md = "---\nname: x\ndescription: d\n---\nbody";
        let role = parse_role_content(Path::new("x.md"), md).expect("parse");
        assert_eq!(role.toolset, None);
        assert_eq!(role.model, None);
        assert_eq!(role.worker, MemberWorker::Internal);
        assert_eq!(role.version, 1);
        assert_eq!(role.resources, MemberResources::default());
    }

    #[test]
    fn worker_variants() {
        for (raw, expect) in [
            ("external:codex", MemberWorker::External("codex".into())),
            ("browser", MemberWorker::Browser),
            ("internal", MemberWorker::Internal),
        ] {
            let md = format!("---\nname: x\ndescription: d\nworker: {raw}\n---\nbody");
            let role = parse_role_content(Path::new("x.md"), &md).expect("parse");
            assert_eq!(role.worker, expect);
        }
        let md = "---\nname: x\ndescription: d\nworker: fax\n---\nbody";
        assert!(matches!(
            parse_role_content(Path::new("x.md"), md),
            Err(RoleParseError::InvalidField(_))
        ));
    }

    #[test]
    fn missing_name_or_frontmatter_fails() {
        assert!(matches!(
            parse_role_content(Path::new("a.md"), "---\ndescription: d\n---\nbody"),
            Err(RoleParseError::InvalidField(_))
        ));
        assert!(matches!(
            parse_role_content(Path::new("a.md"), "没有 frontmatter"),
            Err(RoleParseError::MissingFrontmatter)
        ));
    }

    #[test]
    fn cpu_weight_out_of_job_scale_fails() {
        let md = "---\nname: x\ndescription: d\nresources: {cpu_weight: 30}\n---\nbody";
        assert!(matches!(
            parse_role_content(Path::new("x.md"), md),
            Err(RoleParseError::InvalidField(_))
        ));
    }

    #[test]
    fn library_loads_sorts_and_reports_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("b_second.md"), FULL).expect("write");
        std::fs::write(tmp.path().join("a_first.md"), "---\nname: alpha\ndescription: d\n---\nA body").expect("write");
        std::fs::write(tmp.path().join("broken.md"), "not a role").expect("write");
        let lib = load_role_library(tmp.path());
        assert_eq!(lib.errors.len(), 1, "坏文件应记错而不是 panic");
        let names: Vec<&str> = lib.roles.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "reviewer"], "按文件名排序加载");
        assert!(lib.find("reviewer").is_some());
        assert!(lib.find("ghost").is_none());
    }

    #[test]
    fn library_rejects_duplicate_names() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(tmp.path().join("a.md"), FULL).expect("write");
        std::fs::write(tmp.path().join("b.md"), FULL).expect("write");
        let lib = load_role_library(tmp.path());
        assert_eq!(lib.roles.len(), 1);
        assert_eq!(lib.errors.len(), 1, "同名角色应记错");
    }

    #[test]
    fn builtin_seeds_parse_cleanly() {
        for (name, content) in builtin_role_sources() {
            let role = parse_role_content(Path::new(name), content)
                .unwrap_or_else(|e| panic!("内置角色 {name} 解析失败: {e}"));
            assert!(!role.system_prompt.is_empty(), "{name} 的 prompt 正文不应为空");
            assert!(!role.description.is_empty(), "{name} 的 description 不应为空");
        }
    }

    #[test]
    fn ensure_builtin_writes_once_and_never_overwrites() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = ensure_builtin_roles(tmp.path()).expect("ensure");
        assert_eq!(first, builtin_role_sources().len());
        // 用户改动：改写一个内置角色文件
        let victim = tmp.path().join("coder.md");
        std::fs::write(&victim, "---\nname: coder\ndescription: 我改过的\n---\n自定义").expect("write");
        let second = ensure_builtin_roles(tmp.path()).expect("ensure again");
        assert_eq!(second, 0, "已存在的文件不应重写");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read").contains("我改过的"),
            true,
            "用户改动必须保留"
        );
    }
}
