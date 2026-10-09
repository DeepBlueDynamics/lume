//! Per-run accumulator journal: evicted history stays on disk for exact late rewrites.
use crate::BucketWindow;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};
use ti_contracts::{BackfillLimits, Error, Result};
pub(crate) type WindowKey = (String, u32, u32);
pub(crate) struct WindowJournal {
    file: Option<File>,
    path: PathBuf,
    index: BTreeMap<WindowKey, (u64, usize)>,
    bytes: u64,
    limits: BackfillLimits,
}
impl WindowJournal {
    pub(crate) fn new(root: &str, limits: &BackfillLimits) -> Result<Self> {
        if limits.max_active_bytes == 0
            || limits.max_index_entries == 0
            || limits.max_journal_bytes == 0
        {
            return Err(Error::InvalidInput(
                "sources.backfill limits must be positive".into(),
            ));
        }
        let root = PathBuf::from(root);
        std::fs::create_dir_all(&root)?;
        // Backfill owns the store writer; abandoned per-run scratch is never replayed.
        for entry in std::fs::read_dir(&root)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(".parquet-windows-")
                && entry.file_type()?.is_file()
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|e| Error::InvalidInput(e.to_string()))?
            .as_nanos();
        let path = root.join(format!(".parquet-windows-{}-{stamp}", std::process::id()));
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)?;
        Ok(Self {
            file: Some(file),
            path,
            index: BTreeMap::new(),
            bytes: 0,
            limits: limits.clone(),
        })
    }
    pub(crate) fn load(&mut self, key: &WindowKey) -> Result<BucketWindow> {
        let Some(&(offset, len)) = self.index.get(key) else {
            return Ok(BucketWindow::default());
        };
        self.file
            .as_mut()
            .expect("open journal")
            .seek(SeekFrom::Start(offset))?;
        let mut bytes = vec![0; len];
        self.file
            .as_mut()
            .expect("open journal")
            .read_exact(&mut bytes)?;
        serde_json::from_slice(&bytes)
            .map_err(|e| Error::Corrupt(format!("Parquet accumulator journal: {e}")))
    }
    pub(crate) fn bytes(&self) -> u64 {
        self.bytes
    }
    pub(crate) fn save(&mut self, key: WindowKey, window: &BucketWindow) -> Result<()> {
        if !self.index.contains_key(&key) && self.index.len() >= self.limits.max_index_entries {
            return Err(Error::InvalidInput("Parquet backfill journal index cap reached; import a smaller time range or increase sources.backfill.max_index_entries".into()));
        }
        let bytes = serde_json::to_vec(window).map_err(|e| Error::InvalidInput(e.to_string()))?;
        if self.bytes.saturating_add(bytes.len() as u64) > self.limits.max_journal_bytes {
            return Err(Error::InvalidInput("Parquet backfill scratch cap reached; import a smaller time range or increase sources.backfill.max_journal_bytes".into()));
        }
        self.file
            .as_mut()
            .expect("open journal")
            .seek(SeekFrom::Start(self.bytes))?;
        self.file
            .as_mut()
            .expect("open journal")
            .write_all(&bytes)?;
        self.index.insert(key, (self.bytes, bytes.len()));
        self.bytes += bytes.len() as u64;
        Ok(())
    }
}
impl Drop for WindowJournal {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}
