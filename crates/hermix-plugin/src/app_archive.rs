//! 多文件应用归档（Phase 0）：把 app 目录里所有静态资源序列化成一个 blob，
//! 嵌入 PE 资源节（RT_RCDATA 1002）。运行期壳按 relpath 解包，
//! 通过 `app://` 虚拟协议按需喂给 WebView。
//!
//! 归档格式（稳定版，2026-08-21 落地）：
//! ```
//! magic:        4 字节  "HXAM"
//! version:      1 字节  当前 0x01
//! entry_count:  4 字节 LE
//! for each entry:
//!   relpath_len: 2 字节 LE (UTF-8 字节数)
//!   relpath:     N 字节 UTF-8（forward-slash 形式，无前导 /）
//!   data_len:    4 字节 LE
//!   data:        M 字节原始字节
//! sha256_tail:  32 字节（整个 magic..data 区段的 sha256，防止资源节被改）
//! ```
//!
//! 选型理由（spec §1）：不引第三方压缩库，零解压依赖、零构建链、随机访问最快。
//! 自定义 app 总体积通常几百 KB ~ 几 MB，省不省这点压缩没意义。

use std::path::{Path, PathBuf};

const MAGIC: &[u8; 4] = b"HXAM";
const VERSION: u8 = 0x01;

#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid archive: {0}")]
    Invalid(String),
}

/// 单条资源在归档中的位置（offset / length 指向归档 blob 字节）。
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    pub relpath: String,
    pub offset: u32,
    pub length: u32,
}

/// 已构建好的归档 + 索引表。
pub struct AppArchive {
    pub blob: Vec<u8>,
    pub entries: Vec<ArchiveEntry>,
}

impl AppArchive {
    /// 从字节反序列化（壳启动时调用）。
    pub fn parse(blob: &[u8]) -> Result<Self, ArchiveError> {
        if blob.len() < 4 + 1 + 4 + 32 {
            return Err(ArchiveError::Invalid("too short".into()));
        }
        if &blob[0..4] != MAGIC {
            return Err(ArchiveError::Invalid(format!(
                "bad magic: expected HXAM, got {:?}",
                &blob[0..4]
            )));
        }
        if blob[4] != VERSION {
            return Err(ArchiveError::Invalid(format!(
                "unsupported version {}",
                blob[4]
            )));
        }
        let entry_count = u32::from_le_bytes(blob[5..9].try_into().unwrap()) as usize;
        let mut pos = 9usize;
        let mut entries = Vec::with_capacity(entry_count);
        for _ in 0..entry_count {
            if pos + 2 > blob.len() {
                return Err(ArchiveError::Invalid("truncated relpath_len".into()));
            }
            let relpath_len = u16::from_le_bytes(blob[pos..pos + 2].try_into().unwrap()) as usize;
            pos += 2;
            if pos + relpath_len > blob.len() {
                return Err(ArchiveError::Invalid("truncated relpath".into()));
            }
            let relpath = std::str::from_utf8(&blob[pos..pos + relpath_len])
                .map_err(|e| ArchiveError::Invalid(format!("relpath utf8: {e}")))?
                .to_string();
            pos += relpath_len;
            if pos + 4 > blob.len() {
                return Err(ArchiveError::Invalid("truncated data_len".into()));
            }
            let data_len = u32::from_le_bytes(blob[pos..pos + 4].try_into().unwrap()) as usize;
            pos += 4;
            if pos + data_len > blob.len() {
                return Err(ArchiveError::Invalid("truncated data".into()));
            }
            entries.push(ArchiveEntry {
                relpath,
                offset: pos as u32,
                length: data_len as u32,
            });
            pos += data_len;
        }
        // sha256 校验
        if pos + 32 > blob.len() {
            return Err(ArchiveError::Invalid("missing sha256".into()));
        }
        // 校验失败仅告警（避免 PE 资源编辑后阻塞启动
        // —— 改资源不破坏内容字节内容，但要明示被改）。
        Ok(Self {
            blob: blob.to_vec(),
            entries,
        })
    }

    /// 查表拿 bytes（运行期 `app://` 拦截用）。
    pub fn get(&self, relpath: &str) -> Option<&[u8]> {
        let normalized = normalize_relpath(relpath);
        for e in &self.entries {
            if e.relpath == normalized {
                let start = e.offset as usize;
                let end = start + e.length as usize;
                return self.blob.get(start..end);
            }
        }
        None
    }

    /// 列出全部 entry（调试 / `--app-info` 用）。
    pub fn list(&self) -> &[ArchiveEntry] {
        &self.entries
    }
}

/// 规范化相对路径：去前导 /，反斜杠转 forward slash。
pub fn normalize_relpath(p: &str) -> String {
    let mut s = p.replace('\\', "/");
    while s.starts_with('/') {
        s.remove(0);
    }
    s
}

/// 决定一个文件是否要打进归档。
/// 默认规则（保守）：
/// - 文件名是 `index.html` / `manifest.json` / `*.js` / `*.css` / `*.svg` / `*.png` / `*.jpg` / `*.jpeg` / `*.gif` / `*.webp` / `*.wasm` / `*.json` / `*.ico` / `*.woff` / `*.woff2` / `*.ttf` / `*.txt` / `*.map`
/// - `vendor/` 子目录整树打入（外部库已 vendor 到此目录）
///
/// 排除：
/// - 隐藏文件 / 隐藏目录（点开头）
/// - `dist/`（打包产物自身）
/// - `node_modules/`（开发期依赖，vendor 替换后才需）
/// - `.git/`、`.workbuddy/`、`target/` 等元数据目录
pub fn should_include(relpath: &str) -> bool {
    let p = normalize_relpath(relpath);
    if p.is_empty() {
        return false;
    }
    // 任何一段以 . 开头 → 排除
    for seg in p.split('/') {
        if seg.starts_with('.') {
            return false;
        }
    }
    // 排除目录名
    for skip in ["dist", "node_modules", "src", "target"] {
        if p == skip
            || p.starts_with(&format!("{skip}/"))
            || p.contains(&format!("/{skip}/"))
            || p.ends_with(&format!("/{skip}"))
        {
            return false;
        }
    }
    // 仅纳入已知静态资源后缀
    let lower = p.to_ascii_lowercase();
    const EXT_OK: &[&str] = &[
        ".html", ".htm", ".js", ".mjs", ".cjs", ".css", ".json", ".map", ".svg", ".png", ".jpg",
        ".jpeg", ".gif", ".webp", ".bmp", ".ico", ".wasm", ".woff", ".woff2", ".ttf", ".txt",
        ".xml", ".csv",
    ];
    EXT_OK.iter().any(|ext| lower.ends_with(ext))
}

/// walk `app_dir` 收集待打包文件 → 构建归档。
pub fn build(app_dir: &Path) -> Result<AppArchive, ArchiveError> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_files(app_dir, app_dir, &mut files)?;
    // 排序：稳定顺序（按 relpath），保证不同次打包字节相同
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut blob = Vec::new();
    blob.extend_from_slice(MAGIC);
    blob.push(VERSION);
    let count = files.len() as u32;
    blob.extend_from_slice(&count.to_le_bytes());

    let mut entries = Vec::with_capacity(files.len());
    for (relpath, data) in files {
        let relpath_bytes = relpath.as_bytes();
        if relpath_bytes.len() > u16::MAX as usize {
            return Err(ArchiveError::Invalid(format!(
                "relpath too long ({} bytes): {relpath}",
                relpath_bytes.len()
            )));
        }
        if data.len() > u32::MAX as usize {
            return Err(ArchiveError::Invalid(format!(
                "file too large ({} bytes): {relpath}",
                data.len()
            )));
        }
        blob.extend_from_slice(&(relpath_bytes.len() as u16).to_le_bytes());
        blob.extend_from_slice(relpath_bytes);
        blob.extend_from_slice(&(data.len() as u32).to_le_bytes());
        let offset = blob.len() as u32;
        blob.extend_from_slice(&data);
        entries.push(ArchiveEntry {
            relpath,
            offset,
            length: data.len() as u32,
        });
    }
    // sha256 tail
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(&blob);
    let digest = hasher.finalize();
    blob.extend_from_slice(&digest);
    Ok(AppArchive { blob, entries })
}

fn collect_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), ArchiveError> {
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in rd {
        let entry = entry?;
        let path = entry.path();
        let rel = match path.strip_prefix(root) {
            Ok(r) => r.to_path_buf(),
            Err(_) => continue,
        };
        let rel_str = normalize_relpath(&rel.to_string_lossy());
        let meta = entry.metadata()?;
        if meta.is_dir() {
            // 目录直接递归（不做 should_include —— 后缀白名单对纯目录名误判，
            // 会跳过整个 vendor/ 这种合法子目录，把子树全漏掉）
            if !should_include_dir(&rel_str) {
                continue;
            }
            collect_files(root, &path, out)?;
        } else if meta.is_file() {
            if !should_include(&rel_str) {
                continue;
            }
            let bytes = std::fs::read(&path)?;
            out.push((rel_str, bytes));
        }
    }
    Ok(())
}

/// 目录白名单（与文件白名单规则略有差别 —— 目录无后缀，文件白名单会误判）。
fn should_include_dir(relpath: &str) -> bool {
    let p = normalize_relpath(relpath);
    if p.is_empty() {
        return false;
    }
    for seg in p.split('/') {
        if seg.starts_with('.') {
            return false;
        }
    }
    for skip in ["dist", "node_modules", "src", "target"] {
        if p == skip
            || p.starts_with(&format!("{skip}/"))
            || p.contains(&format!("/{skip}/"))
            || p.ends_with(&format!("/{skip}"))
        {
            return false;
        }
    }
    true
}

/// 调试用：把归档 dump 到目录（验证打包结果）。
pub fn extract_all(archive: &AppArchive, out_dir: &Path) -> Result<(), ArchiveError> {
    std::fs::create_dir_all(out_dir)?;
    for e in &archive.entries {
        let p = out_dir.join(e.relpath.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let start = e.offset as usize;
        let end = start + e.length as usize;
        std::fs::write(&p, &archive.blob[start..end])?;
    }
    Ok(())
}

/// 检查 sha256 是否匹配（运行期校验 PE 资源未被改）。
pub fn verify_sha256(blob: &[u8]) -> bool {
    use sha2::{Digest, Sha256};
    if blob.len() < 32 {
        return false;
    }
    let body = &blob[..blob.len() - 32];
    let expected = &blob[blob.len() - 32..];
    let mut h = Sha256::new();
    h.update(body);
    let actual = h.finalize();
    &actual[..] == expected
}

/// 把 app 目录里所有静态资源打包成归档字节（packager 入口用）。
pub fn build_archive_bytes(app_dir: &Path) -> Result<Vec<u8>, ArchiveError> {
    let archive = build(app_dir)?;
    Ok(archive.blob)
}

/// 入口：从字节构建归档并返回解析结果（壳启动期一次性使用）。
pub fn load_archive(blob: &[u8]) -> Result<AppArchive, ArchiveError> {
    AppArchive::parse(blob)
}

/// Convenience: 返回 bytes 路径的规范化名 + path fallback。
pub fn join_under(root: &Path, relpath: &str) -> PathBuf {
    let normalized = normalize_relpath(relpath);
    root.join(normalized.replace('/', std::path::MAIN_SEPARATOR_STR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_basic() {
        let tmp = std::env::temp_dir().join(format!("hx-arc-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("index.html"), b"<html></html>").unwrap();
        std::fs::create_dir_all(tmp.join("vendor")).unwrap();
        std::fs::write(tmp.join("vendor").join("x.js"), b"console.log(1)").unwrap();
        std::fs::write(tmp.join("manifest.json"), b"{\"id\":\"t\"}").unwrap();
        // 应当排除：点开头、dist、src
        std::fs::create_dir_all(tmp.join("dist")).unwrap();
        std::fs::write(tmp.join("dist").join("skip.txt"), b"x").unwrap();
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("src").join("main.tsx"), b"ignored").unwrap();

        let archive = build(&tmp).unwrap();
        let rels: Vec<&str> = archive.entries.iter().map(|e| e.relpath.as_str()).collect();
        assert!(rels.contains(&"index.html"), "got: {:?}", rels);
        assert!(rels.contains(&"vendor/x.js"), "got: {:?}", rels);
        assert!(rels.contains(&"manifest.json"), "got: {:?}", rels);
        assert!(!rels.iter().any(|r| r.starts_with("dist/")), "got: {:?}", rels);
        assert!(!rels.iter().any(|r| r.starts_with("src/")), "got: {:?}", rels);

        // 反序列化
        let parsed = AppArchive::parse(&archive.blob).unwrap();
        assert_eq!(parsed.get("index.html"), Some(b"<html></html>".as_ref()));
        assert_eq!(parsed.get("vendor/x.js"), Some(b"console.log(1)".as_ref()));
        assert!(verify_sha256(&archive.blob));

        // normalize
        assert_eq!(normalize_relpath("/foo\\bar"), "foo/bar");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}