use anyhow::{ensure, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Checkpoint {
    pub version: u8,
    pub url: String,
    pub size: u64,
    pub etag: Option<String>,
    #[serde(default)]
    pub weak_etag: Option<String>,
    #[serde(default)]
    pub last_modified: Option<String>,
    pub expected_sha256: Option<String>,
    pub chunk_size: u64,
    pub chunks: Vec<Option<String>>,
}

pub fn sidecar(output: &Path, suffix: &str) -> PathBuf {
    let mut path = output.as_os_str().to_os_string();
    path.push(suffix);
    path.into()
}

pub fn lock(output: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(sidecar(output, ".fluxget.lock"))?;
    file.try_lock_exclusive()
        .context("Another fluxget process owns this output")?;
    Ok(file)
}

pub fn load(path: &Path) -> Result<Checkpoint> {
    let file = File::open(path).context("No readable resume checkpoint")?;
    ensure!(
        file.metadata()?.len() <= 32 * 1024 * 1024,
        "Checkpoint exceeds 32 MiB"
    );
    let state: Checkpoint = serde_json::from_reader(file).context("Invalid checkpoint JSON")?;
    ensure!(state.version == 1, "Unsupported checkpoint version");
    ensure!(
        (64 * 1024..=16 * 1024 * 1024).contains(&state.chunk_size),
        "Invalid checkpoint chunk size"
    );
    ensure!(
        state.size.div_ceil(state.chunk_size) == state.chunks.len() as u64,
        "Invalid checkpoint chunk count"
    );
    Ok(state)
}

pub fn save(path: &Path, state: &Checkpoint) -> Result<()> {
    // A fixed name, so a crash between writing and renaming leaves at most one stray file,
    // which the next save simply overwrites. The output lock means there is one writer.
    let temp = sidecar(path, ".tmp");
    let mut options = OpenOptions::new();
    options.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        // The checkpoint stores the final URL, which may carry a signed token.
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp)?;
    serde_json::to_writer(&mut file, state)?;
    file.flush()?;
    file.sync_all()?;
    drop(file);
    // On Windows an antivirus scanner or a checkpoint reader can briefly hold
    // the old file without FILE_SHARE_DELETE. Retry the same atomic replacement;
    // never remove the previous journal as a workaround.
    for attempt in 0..=8 {
        match std::fs::rename(&temp, path) {
            Ok(()) => break,
            Err(error)
                if cfg!(windows)
                    && matches!(error.raw_os_error(), Some(5 | 32 | 33))
                    && attempt < 8 =>
            {
                std::thread::sleep(std::time::Duration::from_millis(10 * (attempt + 1)));
            }
            Err(error) => return Err(error.into()),
        }
    }
    // On Unix also persist the directory entry. Windows replacement is atomic,
    // but power-loss durability still depends on the filesystem's rename policy.
    #[cfg(unix)]
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

pub fn hash(data: &[u8]) -> String {
    format!("{:x}", Sha256::digest(data))
}

pub fn hash_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub fn validate_parts(file: &mut File, state: &mut Checkpoint) -> Result<u64> {
    let mut verified = 0;
    let mut data = vec![0u8; state.chunk_size as usize];
    for (index, expected) in state.chunks.iter_mut().enumerate() {
        if expected.is_none() {
            continue;
        }
        let start = index as u64 * state.chunk_size;
        let len = (state.size - start).min(state.chunk_size) as usize;
        file.seek(SeekFrom::Start(start))?;
        if file.read_exact(&mut data[..len]).is_ok()
            && expected.as_deref() == Some(hash(&data[..len]).as_str())
        {
            verified += len as u64;
        } else {
            *expected = None;
        }
    }
    Ok(verified)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::os::windows::fs::OpenOptionsExt;

    #[test]
    fn checkpoint_replacement_survives_temporary_readers() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("checkpoint.json");
        let state = Checkpoint {
            version: 1,
            url: "https://example.com/file".into(),
            size: 0,
            etag: None,
            weak_etag: None,
            last_modified: None,
            expected_sha256: None,
            chunk_size: 65536,
            chunks: vec![],
        };
        save(&path, &state).unwrap();
        let reader = OpenOptions::new()
            .read(true)
            .share_mode(1 | 2)
            .open(&path)
            .unwrap();
        let reader_thread = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(70));
            drop(reader);
        });
        save(&path, &state).unwrap();
        reader_thread.join().unwrap();
        assert_eq!(load(&path).unwrap().version, 1);
    }
}
