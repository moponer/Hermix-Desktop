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
    let path = snapshot_path(data_dir, &snapshot.volume_state.volume)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("创建索引目录失败：{}", parent.display()))?;
    }

    let tmp = path.with_extension("tmp");
    let bytes = bincode::serialize(snapshot).context("序列化索引快照失败")?;
    fs::write(&tmp, bytes).with_context(|| format!("写入临时快照失败：{}", tmp.display()))?;
    if path.exists() {
        fs::remove_file(&path).with_context(|| format!("替换旧快照失败：{}", path.display()))?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("保存索引快照失败：{}", path.display()))?;
    save_last_volume(data_dir, &snapshot.volume_state.volume)?;

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
