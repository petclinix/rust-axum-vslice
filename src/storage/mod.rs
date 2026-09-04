use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Writes `value` as pretty JSON to `path` via write-tmp + `fsync` + `rename`,
/// so a crash mid-write never leaves a half-written record. The
/// tmp filename is unique per call so concurrent unlocked writers to the same
/// path never share (and corrupt) a tmp file — whichever rename lands last
/// wins, which is the accepted semantics for writes with no invariant to
/// protect (see `docs/architecture.md`'s Design Constraints).
pub fn atomic_write<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let tmp_path = tmp_path_for(path);
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let mut file = File::create(&tmp_path)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);

    fs::rename(&tmp_path, path)?;
    Ok(())
}

/// Reads and deserializes `path`, or `None` if it doesn't exist.
pub fn read_json<T: DeserializeOwned>(path: &Path) -> io::Result<Option<T>> {
    match fs::read(path) {
        Ok(bytes) => {
            let value = serde_json::from_slice(&bytes)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(value))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Reads every `*.json` file directly in `dir` (non-recursive) and
/// deserializes it. An in-progress `.tmp.*` write is skipped, since its
/// filename never has a bare `.json` extension. Missing `dir` yields an
/// empty `Vec`, not an error — a slice that hasn't written anything yet is a
/// normal, not exceptional, state.
pub fn list_dir_json<T: DeserializeOwned>(dir: &Path) -> io::Result<Vec<T>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut items = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if let Some(value) = read_json(&path)? {
            items.push(value);
        }
    }
    Ok(items)
}

/// RAII guard for an OS-level advisory lock (`flock`) taken on a dedicated
/// lock file — this repo's substitute for a database's row locks /
/// transactions (`docs/architecture-internals.md` §1). Dropping the guard
/// closes the file, which releases the
/// lock; there is no explicit unlock step.
pub struct FileLock {
    _file: File,
}

impl FileLock {
    /// Blocks until an exclusive lock on `path` is acquired. Use for any
    /// read-check-then-write critical section (`docs/architecture-internals.md` §1).
    pub fn exclusive(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        file.lock()?;
        Ok(Self { _file: file })
    }

    /// Blocks until a shared lock on `path` is acquired. Multiple readers
    /// may hold a shared lock at once; it excludes concurrent exclusive
    /// locks only.
    pub fn shared(path: &Path) -> io::Result<Self> {
        let file = open_lock_file(path)?;
        file.lock_shared()?;
        Ok(Self { _file: file })
    }

    /// Non-blocking exclusive attempt: `Ok(None)` means another holder has
    /// it right now, rather than blocking the caller.
    pub fn try_exclusive(path: &Path) -> io::Result<Option<Self>> {
        let file = open_lock_file(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(e)) => Err(e),
        }
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

fn tmp_path_for(path: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let pid = std::process::id();
    let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".tmp.{pid}-{nanos}-{seq}"));
    PathBuf::from(tmp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct Record {
        id: String,
        value: u32,
    }

    #[test]
    fn atomic_write_then_read_json_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");
        let record = Record {
            id: "a".into(),
            value: 1,
        };

        atomic_write(&path, &record).unwrap();

        assert_eq!(read_json::<Record>(&path).unwrap(), Some(record));
    }

    #[test]
    fn atomic_write_creates_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/record.json");

        atomic_write(
            &path,
            &Record {
                id: "a".into(),
                value: 1,
            },
        )
        .unwrap();

        assert!(path.exists());
    }

    #[test]
    fn atomic_write_leaves_no_tmp_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");

        atomic_write(
            &path,
            &Record {
                id: "a".into(),
                value: 1,
            },
        )
        .unwrap();

        let leftover: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leftover.is_empty());
    }

    #[test]
    fn atomic_write_overwrite_leaves_only_the_latest_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("record.json");

        atomic_write(
            &path,
            &Record {
                id: "a".into(),
                value: 1,
            },
        )
        .unwrap();
        atomic_write(
            &path,
            &Record {
                id: "a".into(),
                value: 2,
            },
        )
        .unwrap();

        assert_eq!(
            read_json::<Record>(&path).unwrap(),
            Some(Record {
                id: "a".into(),
                value: 2
            })
        );
    }

    #[test]
    fn read_json_missing_file_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.json");

        assert_eq!(read_json::<Record>(&path).unwrap(), None);
    }

    #[test]
    fn list_dir_json_reads_every_record_and_skips_non_json() {
        let dir = tempfile::tempdir().unwrap();
        atomic_write(
            &dir.path().join("a.json"),
            &Record {
                id: "a".into(),
                value: 1,
            },
        )
        .unwrap();
        atomic_write(
            &dir.path().join("b.json"),
            &Record {
                id: "b".into(),
                value: 2,
            },
        )
        .unwrap();
        fs::write(dir.path().join("not-a-record.txt"), b"ignore me").unwrap();

        let mut items = list_dir_json::<Record>(dir.path()).unwrap();
        items.sort_by(|a, b| a.id.cmp(&b.id));

        assert_eq!(
            items,
            vec![
                Record {
                    id: "a".into(),
                    value: 1
                },
                Record {
                    id: "b".into(),
                    value: 2
                },
            ]
        );
    }

    #[test]
    fn list_dir_json_missing_dir_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");

        assert_eq!(list_dir_json::<Record>(&missing).unwrap(), Vec::new());
    }

    #[test]
    fn exclusive_lock_excludes_a_second_exclusive_attempt_until_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("vet.lock");

        let guard = FileLock::exclusive(&lock_path).unwrap();

        // While `guard` is held, a non-blocking attempt must observe it as taken.
        assert!(FileLock::try_exclusive(&lock_path).unwrap().is_none());

        drop(guard);

        // Once released, the same non-blocking attempt must now succeed.
        assert!(FileLock::try_exclusive(&lock_path).unwrap().is_some());
    }

    #[test]
    fn exclusive_lock_blocks_a_concurrent_writer_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("vet.lock");

        let first = FileLock::exclusive(&lock_path).unwrap();
        let (acquired_tx, acquired_rx) = mpsc::channel();

        let waiting_lock_path = lock_path.clone();
        let handle = thread::spawn(move || {
            let _second = FileLock::exclusive(&waiting_lock_path).unwrap();
            acquired_tx.send(()).unwrap();
        });

        // The second thread must still be blocked shortly after `first` was
        // taken — proves `exclusive()` actually blocks rather than
        // succeeding spuriously.
        assert_eq!(
            acquired_rx.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Timeout)
        );

        drop(first);

        acquired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("second thread should acquire the lock once it is released");
        handle.join().unwrap();
    }

    #[test]
    fn shared_locks_do_not_exclude_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let lock_path = dir.path().join("vet.lock");

        let first = FileLock::shared(&lock_path).unwrap();
        let second = FileLock::shared(&lock_path).unwrap();

        drop(first);
        drop(second);
    }
}
