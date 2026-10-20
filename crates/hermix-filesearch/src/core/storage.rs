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
    use super::sanitize_volume;

    #[test]
    fn sanitizes_volume_for_snapshot_filename() {
        assert_eq!(sanitize_volume("F:"), "F");
        assert_eq!(sanitize_volume("C:\\"), "C");
    }
}
