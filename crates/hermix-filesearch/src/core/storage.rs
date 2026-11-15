use std::{fs, path::{Path, PathBuf}};

use anyhow::{Context, Result};

use super::model::{IndexSnapshot, SNAPSHOT_VERSION};

pub struct LoadedSnapshot {
    pub snapshot: IndexSnapshot,
    pub path: PathBuf,
}

pub fn load_last_snapshot(data_dir: &Path) -> Result<Option<LoadedSnapshot>> {
    let Some(volume) = load_last_volume(data_dir)? else {
        return Ok(None);
    };
    load_snapshot(data_dir, &volume)
}

pub fn load_snapshot(data_dir: &Path, volume: &str) -> Result<Option<LoadedSnapshot>> {
    let path = snapshot_path(data_dir, volume)?;
    if !path.exists() {
        return Ok(None);
    }

    let bytes = fs::read(&path).with_context(|| format!("读取索引快照失败：{}", path.display()))?;
    let snapshot: IndexSnapshot = bincode::deserialize(&bytes)
        .with_context(|| format!("解析索引快照失败：{}", path.display()))?;

    if snapshot.version != SNAPSHOT_VERSION {
        return Ok(None);
    }

    Ok(Some(LoadedSnapshot { snapshot, path }))
}

pub fn save_snapshot(data_dir: &Path, snapshot: &IndexSnapshot) -> Result<PathBuf> {
    save_snapshot_parts(data_dir, &snapshot.volume_state, &snapshot.entries)
}

/// 借引用落盘：调用方无需把整份索引 clone 出来（百万级条目一次 clone
/// 就是几百 MB 的瞬时峰值）。
pub fn save_snapshot_parts(
    data_dir: &Path,
    volume_state: &super::model::VolumeIndexState,
    entries: &[super::model::IndexEntry],
) -> Result<PathBuf> {
    #[derive(serde::Serialize)]
    struct SnapshotView<'a> {
        version: u32,
        volume_state: &'a super::model::VolumeIndexState,
        entries: &'a [super::model::IndexEntry],
    }
    let view = SnapshotView {
        version: SNAPSHOT_VERSION,
        volume_state,
        entries,
    };

    let path = snapshot_path(data_dir, &volume_state.volume)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建索引目录失败：{}", parent.display()))?;
    }

    let tmp = path.with_extension("tmp");
    // 流式写：`bincode::serialize` 会把整个索引再复制进一个 Vec<u8>，
    // 百万级条目时峰值内存凭空翻倍（实测整机 1.4GB 常驻）。直接写文件。
    {
        use std::io::Write;
        let file = fs::File::create(&tmp)
            .with_context(|| format!("创建临时快照失败：{}", tmp.display()))?;
        let mut writer = std::io::BufWriter::new(file);
        bincode::serialize_into(&mut writer, &view).context("序列化索引快照失败")?;
        writer
            .flush()
            .with_context(|| format!("刷新临时快照失败：{}", tmp.display()))?;
    }
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("替换旧快照失败：{}", path.display()))?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("保存索引快照失败：{}", path.display()))?;
    save_last_volume(data_dir, &volume_state.volume)?;

    Ok(path)
}

// ---------------------------------------------------------------------------
// 增量日志（照搬 Everything：db + 回放journal，而不是每次重写整份 db）
// ---------------------------------------------------------------------------

/// 一条增量批次：`next_usn` 是应用完这批记录后的游标。
#[derive(serde::Serialize, serde::Deserialize)]
struct DeltaBatch {
    next_usn: i64,
    records: Vec<super::model::NtfsRecord>,
}

pub fn delta_path(data_dir: &Path, volume: &str) -> Result<PathBuf> {
    let mut dir = data_dir.to_path_buf();
    dir.push("indexes");
    dir.push(format!("{}.delta", sanitize_volume(volume)));
    Ok(dir)
}

/// 追加一批 USN 记录。格式：`u32 长度 + bincode(DeltaBatch)`，只追加不改写。
pub fn append_delta(
    data_dir: &Path,
    volume: &str,
    next_usn: i64,
    records: &[super::model::NtfsRecord],
) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    use std::io::Write;

    let path = delta_path(data_dir, volume)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建索引目录失败：{}", parent.display()))?;
    }
    let bytes = bincode::serialize(&DeltaBatch {
        next_usn,
        records: records.to_vec(),
    })
    .context("序列化增量批次失败")?;

    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("打开增量日志失败：{}", path.display()))?;
    file.write_all(&(bytes.len() as u32).to_le_bytes())
        .and_then(|_| file.write_all(&bytes))
        .with_context(|| format!("写入增量日志失败：{}", path.display()))?;
    Ok(())
}

/// 读出全部增量批次（按写入顺序）。文件损坏时保留已解析部分——宁可少几条
/// 增量，也不要整卷重建。
pub fn load_deltas(
    data_dir: &Path,
    volume: &str,
) -> Result<Vec<(i64, Vec<super::model::NtfsRecord>)>> {
    use std::io::Read;

    let path = delta_path(data_dir, volume)?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = fs::File::open(&path)
        .with_context(|| format!("打开增量日志失败：{}", path.display()))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .with_context(|| format!("读取增量日志失败：{}", path.display()))?;

    let mut batches = Vec::new();
    let mut offset = 0usize;
    while offset + 4 <= bytes.len() {
        let len = u32::from_le_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        offset += 4;
        if len == 0 || offset + len > bytes.len() {
            break;
        }
        match bincode::deserialize::<DeltaBatch>(&bytes[offset..offset + len]) {
            Ok(batch) => batches.push((batch.next_usn, batch.records)),
            Err(_) => break,
        }
        offset += len;
    }
    Ok(batches)
}

pub fn delta_size(data_dir: &Path, volume: &str) -> u64 {
    delta_path(data_dir, volume)
        .ok()
        .and_then(|path| fs::metadata(path).ok())
        .map(|metadata| metadata.len())
        .unwrap_or(0)
}

/// 压缩（compaction）后清空增量日志：快照已经包含全部条目。
pub fn clear_delta(data_dir: &Path, volume: &str) -> Result<()> {
    let path = delta_path(data_dir, volume)?;
    if path.exists() {
        fs::remove_file(&path)
            .with_context(|| format!("清空增量日志失败：{}", path.display()))?;
    }
    Ok(())
}

pub fn snapshot_path(data_dir: &Path, volume: &str) -> Result<PathBuf> {
    let mut dir = data_dir.to_path_buf();
    dir.push("indexes");
    dir.push(format!("{}.bin", sanitize_volume(volume)));
    Ok(dir)
}

fn settings_path(data_dir: &Path) -> Result<PathBuf> {
    let mut dir = data_dir.to_path_buf();
    fs::create_dir_all(&dir).with_context(|| format!("创建应用数据目录失败：{}", dir.display()))?;
    dir.push("last-volume.txt");
    Ok(dir)
}

fn save_last_volume(data_dir: &Path, volume: &str) -> Result<()> {
    let path = settings_path(data_dir)?;
    fs::write(&path, volume).with_context(|| format!("保存上次选择卷失败：{}", path.display()))
}

fn load_last_volume(data_dir: &Path) -> Result<Option<String>> {
    let path = settings_path(data_dir)?;
    if !path.exists() {
        return Ok(None);
    }
    let volume = fs::read_to_string(&path)
        .with_context(|| format!("读取上次选择卷失败：{}", path.display()))?
        .trim()
        .to_string();
    if volume.is_empty() {
        Ok(None)
    } else {
        Ok(Some(volume))
    }
}

fn sanitize_volume(volume: &str) -> String {
    volume
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::model::NtfsRecord;

    fn record(file_ref: u64, name: &str) -> NtfsRecord {
        NtfsRecord {
            file_ref,
            parent_ref: 5,
            name: name.to_string(),
            attributes: 0,
            usn: 1,
            timestamp: 0,
            reason: 0,
            size: 0,
        }
    }

    /// 增量日志：只追加、按顺序回放、压缩后清空。
    #[test]
    fn delta_log_appends_replays_and_clears() {
        let dir = tempfile::tempdir().unwrap();
        append_delta(dir.path(), "C:", 100, &[record(1, "a.txt")]).unwrap();
        append_delta(dir.path(), "C:", 120, &[record(2, "b.txt")]).unwrap();

        let batches = load_deltas(dir.path(), "C:").unwrap();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].0, 100);
        assert_eq!(batches[0].1[0].name, "a.txt");
        assert_eq!(batches[1].0, 120);
        assert_eq!(batches[1].1[0].name, "b.txt");
        assert!(delta_size(dir.path(), "C:") > 0);

        clear_delta(dir.path(), "C:").unwrap();
        assert!(load_deltas(dir.path(), "C:").unwrap().is_empty());
        assert_eq!(delta_size(dir.path(), "C:"), 0);
    }

    /// 空批次不写文件（USN 游标推进也要留痕，但没记录时不写）。
    #[test]
    fn empty_batch_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        append_delta(dir.path(), "D:", 1, &[]).unwrap();
        assert_eq!(delta_size(dir.path(), "D:"), 0);
    }

    #[test]
    fn sanitizes_volume_for_snapshot_filename() {
        assert_eq!(sanitize_volume("F:"), "F");
        assert_eq!(sanitize_volume("C:\\"), "C");
    }
}
