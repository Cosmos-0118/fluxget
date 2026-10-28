//! Downloads that stopped part-way, found again so `fx resume` can list them.
//!
//! A small index of started downloads is kept in the user's data folder, and the
//! Downloads folder and current folder are scanned too, so downloads made by older
//! versions or in another place are found as well. Nothing here changes a download.

use crate::{
    location,
    state::{self, Checkpoint},
    ui,
};
use anyhow::Result;
use fs2::FileExt;
use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    time::SystemTime,
};

const CHECKPOINT: &str = ".fluxget.json";

pub struct Paused {
    pub output: PathBuf,
    pub url: String,
    pub done: u64,
    pub size: u64,
    pub modified: SystemTime,
}

fn index_path() -> Option<PathBuf> {
    let base = std::env::var_os("FLUXGET_STATE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::data_local_dir().map(|dir| dir.join("fluxget")))?;
    Some(base.join("paused.json"))
}

fn read_index() -> BTreeSet<PathBuf> {
    index_path()
        .and_then(|path| fs::read(path).ok())
        .and_then(|data| serde_json::from_slice::<Vec<PathBuf>>(&data).ok())
        .map(|list| list.into_iter().collect())
        .unwrap_or_default()
}

fn write_index(set: &BTreeSet<PathBuf>) -> Result<()> {
    let Some(path) = index_path() else {
        return Ok(());
    };
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(dir)?;
    let mut file = tempfile::NamedTempFile::new_in(dir)?;
    serde_json::to_writer(&mut file, &set.iter().collect::<Vec<_>>())?;
    file.persist(path)?;
    Ok(())
}

/// Notes a download that has started, so it can be listed if it gets interrupted.
pub fn remember(output: &Path) {
    if let Ok(path) = std::path::absolute(output) {
        let mut set = read_index();
        if set.insert(path) {
            let _ = write_index(&set);
        }
    }
}

/// Drops a download that finished.
pub fn forget(output: &Path) {
    if let Ok(path) = std::path::absolute(output) {
        let mut set = read_index();
        if set.remove(&path) {
            let _ = write_index(&set);
        }
    }
}

/// Another running fluxget holds the lock beside its output.
fn is_active(output: &Path) -> bool {
    OpenOptions::new()
        .read(true)
        .write(true)
        .open(state::sidecar(output, ".fluxget.lock"))
        .is_ok_and(|lock| lock.try_lock_exclusive().is_err())
}

fn inspect(output: &Path) -> Option<Paused> {
    if output.exists() || !state::sidecar(output, ".fluxget.part").exists() {
        return None;
    }
    let checkpoint_path = state::sidecar(output, CHECKPOINT);
    let checkpoint: Checkpoint = state::load(&checkpoint_path).ok()?;
    if is_active(output) {
        return None;
    }
    let finished = checkpoint.chunks.iter().filter(|c| c.is_some()).count() as u64;
    Some(Paused {
        output: output.to_path_buf(),
        url: checkpoint.url,
        done: (finished * checkpoint.chunk_size).min(checkpoint.size),
        size: checkpoint.size,
        modified: fs::metadata(&checkpoint_path).ok()?.modified().ok()?,
    })
}

/// Every interrupted download that can be resumed, most recent first.
pub fn discover() -> Vec<Paused> {
    let mut candidates = read_index();
    let here = std::env::current_dir().unwrap_or_default();
    for dir in [location::downloads_dir(), here] {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some(stem) = name.strip_suffix(CHECKPOINT) {
                if let Ok(path) = std::path::absolute(entry.path().with_file_name(stem)) {
                    candidates.insert(path);
                }
            }
        }
    }
    let mut found: Vec<Paused> = candidates
        .into_iter()
        .filter_map(|output| inspect(&output))
        .collect();
    found.sort_by(|a, b| b.modified.cmp(&a.modified));
    // Forget entries that finished or were deleted.
    let _ = write_index(&found.iter().map(|p| p.output.clone()).collect());
    found
}

fn describe(item: &Paused) -> String {
    let name = item
        .output
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let folder = item
        .output
        .parent()
        .map_or_else(String::new, |p| p.display().to_string());
    let percent = if item.size == 0 {
        0
    } else {
        item.done * 100 / item.size
    };
    format!(
        "{name}   {percent:>3}%   {} of {}   {folder}",
        ui::bytes(item.done),
        ui::bytes(item.size)
    )
}

/// Lists what can be resumed for a terminal-less caller.
pub fn listing(found: &[Paused]) -> String {
    found
        .iter()
        .map(|item| format!("  {}\n    {}", describe(item), item.url))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Arrow-key choice among paused downloads; `None` when cancelled.
pub fn choose(found: &[Paused]) -> Result<Option<usize>> {
    let items: Vec<String> = found.iter().map(describe).collect();
    let width = dialoguer::console::Term::stderr().size().1.max(40) as usize;
    let items: Vec<String> = items
        .iter()
        .map(|text| {
            dialoguer::console::truncate_str(text, width.saturating_sub(8), "...").into_owned()
        })
        .collect();
    eprintln!(
        "\n  {} paused download{}   ·   ↑/↓ move   ·   Enter resume   ·   Esc cancel\n",
        found.len(),
        if found.len() == 1 { "" } else { "s" }
    );
    Ok(dialoguer::Select::with_theme(&ui::theme())
        .with_prompt("Resume which download")
        .items(&items)
        .default(0)
        .max_length(12)
        .interact_opt()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_downloads_are_found_and_finished_ones_are_not() {
        let dir = tempfile::tempdir().unwrap();
        let output = dir.path().join("big.iso");
        let checkpoint = Checkpoint {
            version: 1,
            url: "https://example.org/big.iso".into(),
            size: 10 * 65536,
            etag: Some("\"x\"".into()),
            expected_sha256: None,
            chunk_size: 65536,
            chunks: (0..10).map(|i| (i < 4).then(|| "h".into())).collect(),
        };
        state::save(&state::sidecar(&output, CHECKPOINT), &checkpoint).unwrap();
        fs::write(state::sidecar(&output, ".fluxget.part"), b"x").unwrap();
        let item = inspect(&output).expect("paused");
        assert_eq!((item.done, item.size), (4 * 65536, 10 * 65536));
        assert!(describe(&item).contains("40%"), "{}", describe(&item));
        fs::write(&output, b"done").unwrap();
        assert!(
            inspect(&output).is_none(),
            "a finished output is not paused"
        );
    }
}
