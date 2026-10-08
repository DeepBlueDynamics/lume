//! D52 transaction log. All publication/recovery is protected by documents.lock.
use super::{Operation, StoredDocument};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use ti_contracts::{Document, Error, Result};

const MAGIC: &[u8; 8] = b"LUMEDOC\0";
const FORMAT: u32 = 1;
const HEADER: u64 = 24;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

pub(super) struct Persistence {
    directory: PathBuf,
    generation: Option<u64>,
    offset: u64,
    pub total_operations: u64,
    pub torn: bool,
}
pub(super) struct Replay {
    pub reset: bool,
    pub operations: Vec<Operation>,
}
pub(super) struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the owned handle releases the lock even if unlock fails.
        let _ = self.0.unlock();
    }
}
fn corrupt(message: impl Into<String>) -> Error {
    Error::Corrupt(message.into())
}
fn sync_directory(directory: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(directory)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = directory;
    Ok(())
}
fn temp_path(path: &Path) -> PathBuf {
    path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ))
}
fn header(generation: u64) -> Vec<u8> {
    let mut bytes = MAGIC.to_vec();
    bytes.extend_from_slice(&FORMAT.to_le_bytes());
    bytes.extend_from_slice(&generation.to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
    bytes
}
fn frame(operations: &[Operation]) -> Result<Vec<u8>> {
    let payload = serde_json::to_vec(operations)
        .map_err(|e| corrupt(format!("document transaction encoding: {e}")))?;
    let length = (payload.len() as u64).to_le_bytes();
    let mut hash = crc32fast::Hasher::new();
    hash.update(&length);
    hash.update(&payload);
    let mut bytes = length.to_vec();
    bytes.extend_from_slice(&hash.finalize().to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(&bytes).to_le_bytes());
    bytes.extend_from_slice(&payload);
    Ok(bytes)
}

impl Persistence {
    pub fn new(root: &Path) -> Self {
        Self {
            directory: root.join("docs"),
            generation: None,
            offset: 0,
            total_operations: 0,
            torn: false,
        }
    }
    fn path(&self) -> PathBuf {
        self.directory.join("documents.log")
    }
    pub fn invalidate(&mut self) {
        self.generation = None;
        self.offset = 0;
    }
    pub fn writer_lock(&self) -> Result<Lock> {
        fs::create_dir_all(&self.directory)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.directory.join("documents.lock"))?;
        file.lock()?;
        Ok(Lock(file))
    }
    pub fn reader_lock(&self) -> Result<Option<Lock>> {
        if !self.path().exists() {
            return Ok(None);
        }
        let file = File::open(self.directory.join("documents.lock"))?;
        file.lock_shared()?;
        Ok(Some(Lock(file)))
    }

    pub fn migrate(&mut self) -> Result<()> {
        let legacy = self.directory.join("documents.json");
        if !legacy.exists() {
            return Ok(());
        }
        let _lock = self.writer_lock()?;
        if !legacy.exists() {
            return Ok(());
        }
        let backup = self.directory.join("documents.json.bak");
        if self.path().exists() {
            // Finish only our interrupted migration, never silently discard a
            // legacy writer's divergent snapshot.
            if backup.exists() && fs::read(&legacy)? == fs::read(&backup)? {
                fs::remove_file(legacy)?;
                sync_directory(&self.directory)?;
                return Ok(());
            }
            return Err(corrupt(
                "legacy document snapshot beside an active D52 log; restore explicitly",
            ));
        }
        let bytes = fs::read(&legacy)?;
        let documents: Vec<StoredDocument> = serde_json::from_slice(&bytes)
            .map_err(|e| corrupt(format!("legacy documents.json: {e}")))?;
        for document in &documents {
            Document::from(document.clone()).validate()?;
        }
        if backup.exists() {
            if fs::read(&backup)? != bytes {
                return Err(corrupt(
                    "documents.json.bak already exists with different content",
                ));
            }
        } else {
            let temporary = temp_path(&backup);
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(temporary, &backup)?;
            sync_directory(&self.directory)?;
        }
        let operations = documents
            .into_iter()
            .map(Operation::Upsert)
            .collect::<Vec<_>>();
        self.replace_locked(1, &operations)?;
        fs::remove_file(legacy)?;
        sync_directory(&self.directory)?;
        Ok(())
    }

    pub fn replay_locked(&mut self, repair: bool) -> Result<Replay> {
        let path = self.path();
        let mut file = match File::open(&path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let reset = self.generation.is_some();
                self.invalidate();
                self.total_operations = 0;
                self.torn = false;
                return Ok(Replay {
                    reset,
                    operations: vec![],
                });
            }
            Err(e) => return Err(e.into()),
        };
        let length = file.metadata()?.len();
        let mut bytes = [0u8; HEADER as usize];
        file.read_exact(&mut bytes)
            .map_err(|e| corrupt(format!("document log header: {e}")))?;
        if &bytes[..8] != MAGIC
            || crc32fast::hash(&bytes[..20])
                != u32::from_le_bytes(bytes[20..24].try_into().unwrap())
        {
            return Err(corrupt("invalid document log header"));
        }
        let format = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if format != FORMAT {
            return Err(corrupt(format!("unsupported document log version {format}; do not downgrade without restoring .bak")));
        }
        let generation = u64::from_le_bytes(bytes[12..20].try_into().unwrap());
        let reset = self.generation != Some(generation) || length < self.offset;
        let mut offset = if reset { HEADER } else { self.offset };
        let mut total = if reset { 0 } else { self.total_operations };
        file.seek(SeekFrom::Start(offset))?;
        let mut operations = Vec::new();
        let mut torn = false;
        while offset < length {
            if length - offset < 16 {
                torn = true;
                break;
            }
            let mut prefix = [0u8; 16];
            file.read_exact(&mut prefix)?;
            if crc32fast::hash(&prefix[..12])
                != u32::from_le_bytes(prefix[12..16].try_into().unwrap())
            {
                return Err(corrupt(format!(
                    "document frame header checksum mismatch at offset {offset}"
                )));
            }
            let payload_length = u64::from_le_bytes(prefix[..8].try_into().unwrap());
            // Never allocate from an unchecked declared length.
            if payload_length > length - offset - 16 {
                torn = true;
                break;
            }
            let size = usize::try_from(payload_length)
                .map_err(|_| corrupt("document frame too large for this platform"))?;
            let mut payload = vec![0; size];
            file.read_exact(&mut payload)?;
            let mut hash = crc32fast::Hasher::new();
            hash.update(&prefix[..8]);
            hash.update(&payload);
            if hash.finalize() != u32::from_le_bytes(prefix[8..12].try_into().unwrap()) {
                if offset + 16 + payload_length == length {
                    torn = true;
                    break;
                }
                return Err(corrupt(format!(
                    "document checksum mismatch at offset {offset}"
                )));
            }
            let batch: Vec<Operation> = serde_json::from_slice(&payload)
                .map_err(|e| corrupt(format!("document transaction at {offset}: {e}")))?;
            for operation in &batch {
                operation.validate()?;
            }
            total += batch.len() as u64;
            operations.extend(batch);
            offset += 16 + payload_length;
        }
        drop(file); // Windows replacement/repair must not retain a read handle.
        if torn && repair {
            let file = OpenOptions::new().write(true).open(&path)?;
            file.set_len(offset)?;
            file.sync_all()?;
            torn = false;
        }
        self.generation = Some(generation);
        self.offset = offset;
        self.total_operations = total;
        self.torn = torn;
        Ok(Replay { reset, operations })
    }

    pub fn append_locked(&mut self, operations: &[Operation]) -> Result<()> {
        if !self.path().exists() {
            return self.replace_locked(1, operations);
        }
        let bytes = frame(operations)?;
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new().append(true).open(self.path())?;
            #[cfg(test)]
            if WRITE_FAULT.with(|fault| fault.get()) == 1 {
                file.write_all(&bytes[..bytes.len() / 2])?;
                return Err(std::io::Error::other("injected partial document append").into());
            }
            file.write_all(&bytes)?;
            #[cfg(test)]
            if WRITE_FAULT.with(|fault| fault.get()) == 2 {
                return Err(std::io::Error::other("injected document fsync failure").into());
            }
            file.sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            self.invalidate();
            return Err(error);
        }
        self.offset += bytes.len() as u64;
        self.total_operations += operations.len() as u64;
        Ok(())
    }
    pub fn compact_locked<'a>(
        &mut self,
        documents: impl Iterator<Item = &'a Document>,
        live_count: usize,
    ) -> Result<()> {
        if self.total_operations <= (live_count as u64).saturating_mul(2) {
            return Ok(());
        }
        let operations = documents
            .map(|doc| Operation::Upsert(StoredDocument::from(doc)))
            .collect::<Vec<_>>();
        let generation = self
            .generation
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| corrupt("document generation overflow"))?;
        self.replace_locked(generation, &operations)
    }
    fn replace_locked(&mut self, generation: u64, operations: &[Operation]) -> Result<()> {
        let path = self.path();
        let temporary = temp_path(&path);
        let bytes = frame(operations)?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(&header(generation))?;
        checkpoint("partial_temp");
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        checkpoint("synced_temp");
        fs::rename(&temporary, &path)?;
        checkpoint("renamed");
        if let Err(error) = sync_directory(&self.directory) {
            self.invalidate();
            return Err(error);
        }
        self.generation = Some(generation);
        self.offset = HEADER + bytes.len() as u64;
        self.total_operations = operations.len() as u64;
        self.torn = false;
        Ok(())
    }
}

#[cfg(test)]
thread_local! { pub(super) static WRITE_FAULT: std::cell::Cell<u8> = const { std::cell::Cell::new(0) }; }

#[cfg(not(test))]
fn checkpoint(_name: &str) {}
#[cfg(test)]
fn checkpoint(name: &str) {
    // Only a specially launched test subprocess can activate these hooks.
    if std::env::var("LUME_DOCSTORE_CRASH_PHASE").as_deref() == Ok(name) {
        if let Some(marker) = std::env::var_os("LUME_DOCSTORE_CRASH_MARKER") {
            fs::write(marker, name).unwrap();
            loop {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
    }
}
