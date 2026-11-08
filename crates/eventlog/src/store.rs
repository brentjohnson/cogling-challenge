//! Writing logs and where they go.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use cogling_protocol::GameId;

use crate::{Event, EventBody, Header, LogError};

/// Destination for one game's events.
pub trait LogSink: Send {
    /// Appends an event, assigning the next sequence number, which is returned.
    ///
    /// # Errors
    ///
    /// [`LogError`] if writing fails.
    fn append(&mut self, t_ms: u64, body: EventBody) -> Result<u64, LogError>;

    /// Flushes and closes the log.
    ///
    /// # Errors
    ///
    /// [`LogError`] if flushing fails.
    fn close(self: Box<Self>) -> Result<(), LogError>;
}

/// Creates a [`LogSink`] per game.
///
/// Standalone mode uses [`FileLogStore`]; hosted mode will add a database /
/// object-storage implementation behind the same trait.
pub trait LogStore: Send + Sync {
    /// Starts a new log with `header`.
    ///
    /// # Errors
    ///
    /// [`LogError`] if the log cannot be created.
    fn create(&self, header: &Header) -> Result<Box<dyn LogSink>, LogError>;
}

/// Writes JSON Lines to any [`Write`], numbering events.
#[derive(Debug)]
pub struct LogWriter<W: Write> {
    out: W,
    next_seq: u64,
}

impl<W: Write> LogWriter<W> {
    /// Writes the header line.
    ///
    /// # Errors
    ///
    /// [`LogError`] if writing fails.
    pub fn new(mut out: W, header: &Header) -> Result<Self, LogError> {
        write_line(&mut out, header)?;
        Ok(Self { out, next_seq: 1 })
    }

    /// Appends an event and returns its sequence number.
    ///
    /// # Errors
    ///
    /// [`LogError`] if writing fails.
    pub fn append(&mut self, t_ms: u64, body: EventBody) -> Result<u64, LogError> {
        let seq = self.next_seq;
        let event = Event { seq, t_ms, body };
        write_line(&mut self.out, &event)?;
        self.next_seq += 1;
        Ok(seq)
    }

    /// Flushes and returns the underlying writer.
    ///
    /// # Errors
    ///
    /// [`LogError`] if flushing fails.
    pub fn finish(mut self) -> Result<W, LogError> {
        self.out.flush()?;
        Ok(self.out)
    }
}

fn write_line<W: Write, T: serde::Serialize>(out: &mut W, value: &T) -> Result<(), LogError> {
    serde_json::to_writer(&mut *out, value).map_err(|e| LogError::Format(e.to_string()))?;
    out.write_all(b"\n")?;
    Ok(())
}

impl<W: Write + Send> LogSink for LogWriter<W> {
    fn append(&mut self, t_ms: u64, body: EventBody) -> Result<u64, LogError> {
        Self::append(self, t_ms, body)
    }

    fn close(self: Box<Self>) -> Result<(), LogError> {
        self.finish().map(drop)
    }
}

/// Writes `<dir>/<game>-<game_id>.jsonl` per game.
#[derive(Debug, Clone)]
pub struct FileLogStore {
    dir: PathBuf,
}

impl FileLogStore {
    /// Uses (and creates if needed) `dir`.
    ///
    /// # Errors
    ///
    /// [`LogError::Io`] if the directory cannot be created.
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self, LogError> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// The directory logs are written to.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The path a game's log is written to.
    #[must_use]
    pub fn path_for(&self, game: &str, game_id: GameId) -> PathBuf {
        self.dir.join(format!("{game}-{game_id}.jsonl"))
    }
}

impl LogStore for FileLogStore {
    fn create(&self, header: &Header) -> Result<Box<dyn LogSink>, LogError> {
        let file = File::create_new(self.path_for(&header.game, header.game_id))?;
        Ok(Box::new(LogWriter::new(BufWriter::new(file), header)?))
    }
}

/// Discards everything; for runs where logging is off.
#[derive(Debug, Clone, Copy, Default)]
pub struct NullLogStore;

struct NullSink(u64);

impl LogSink for NullSink {
    fn append(&mut self, _t_ms: u64, _body: EventBody) -> Result<u64, LogError> {
        self.0 += 1;
        Ok(self.0)
    }

    fn close(self: Box<Self>) -> Result<(), LogError> {
        Ok(())
    }
}

impl LogStore for NullLogStore {
    fn create(&self, _header: &Header) -> Result<Box<dyn LogSink>, LogError> {
        Ok(Box::new(NullSink(0)))
    }
}

/// Keeps finished logs in memory; useful for tests and embedding.
#[derive(Debug, Clone, Default)]
pub struct MemoryLogStore {
    logs: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl MemoryLogStore {
    /// An empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The raw bytes of every closed log, in close order.
    #[must_use]
    pub fn logs(&self) -> Vec<Vec<u8>> {
        self.logs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

struct MemorySink {
    writer: LogWriter<Vec<u8>>,
    done: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl LogSink for MemorySink {
    fn append(&mut self, t_ms: u64, body: EventBody) -> Result<u64, LogError> {
        self.writer.append(t_ms, body)
    }

    fn close(self: Box<Self>) -> Result<(), LogError> {
        let bytes = self.writer.finish()?;
        self.done
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(bytes);
        Ok(())
    }
}

impl LogStore for MemoryLogStore {
    fn create(&self, header: &Header) -> Result<Box<dyn LogSink>, LogError> {
        Ok(Box::new(MemorySink {
            writer: LogWriter::new(Vec::new(), header)?,
            done: Arc::clone(&self.logs),
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use cogling_game_core::Seat;
    use serde_json::json;

    use super::*;
    use crate::GameLog;
    use crate::tests::header;

    fn exercise(store: &dyn LogStore, h: &Header) {
        let mut sink = store.create(h).unwrap();
        assert_eq!(
            sink.append(0, EventBody::Disconnect { seat: Seat::FIRST })
                .unwrap(),
            1
        );
        assert_eq!(
            sink.append(5, EventBody::Reconnect { seat: Seat::FIRST })
                .unwrap(),
            2
        );
        sink.close().unwrap();
    }

    #[test]
    fn file_store_writes_readable_logs() {
        let dir = std::env::temp_dir().join(format!("cogling-eventlog-test-{}", GameId::new()));
        let store = FileLogStore::new(&dir).unwrap();
        let h = header(json!({}), 1);
        exercise(&store, &h);
        let path = store.path_for(&h.game, h.game_id);
        let log = GameLog::read(std::io::BufReader::new(File::open(&path).unwrap())).unwrap();
        assert_eq!(log.events().len(), 2);
        // Never overwrite an existing log.
        assert!(store.create(&h).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn memory_and_null_stores() {
        let mem = MemoryLogStore::new();
        exercise(&mem, &header(json!({}), 1));
        let logs = mem.logs();
        assert_eq!(logs.len(), 1);
        let log = GameLog::read(Cursor::new(&logs[0])).unwrap();
        assert_eq!(log.events().len(), 2);
        exercise(&NullLogStore, &header(json!({}), 1));
    }
}
