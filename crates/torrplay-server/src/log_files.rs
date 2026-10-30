// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The rotating log files `docs/observability.md` describes: JSON lines in
//! one directory, written by a background thread and read back, newest
//! first, for `/api/v1/log-records`.
//!
//! The current file is `torrplay.jsonl`. When a record would take it past
//! [`FILE_MAX_BYTES`], it becomes `torrplay.1.jsonl`, each older file moves
//! up one number, and the oldest beyond [`FILES`] is deleted. Logging never
//! waits for the disk: records go through a bounded queue, and those the
//! queue or the disk cannot take are dropped and counted.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// The largest a file grows before it rotates.
pub const FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// The number of files kept, the current one included.
pub const FILES: usize = 4;

/// The records waiting for the writer, beyond which records are dropped.
const QUEUE_RECORDS: usize = 1024;

/// The current file's name.
const CURRENT: &str = "torrplay.jsonl";

/// One log record, in the order its members are written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    /// Its position among every record the files have held.
    pub sequence: u64,
    /// When it was logged, in RFC 3339 to the millisecond. Files written
    /// before 2026-10-30 name it `time`.
    #[serde(alias = "time")]
    pub logged_at: String,
    /// `error`, `warn`, `info`, `debug`, or `trace`.
    pub level: String,
    /// The run that logged it.
    pub run_id: String,
    /// The request it was logged for, if any.
    pub request_id: Option<String>,
    /// The module that logged it.
    pub target: String,
    /// The message.
    pub message: String,
    /// The structured fields, as text.
    pub fields: BTreeMap<String, String>,
}

/// The name of the file `index` places from the current one, which is 0.
fn file_name(index: usize) -> String {
    match index {
        0 => CURRENT.to_owned(),
        index => format!("torrplay.{index}.jsonl"),
    }
}

/// The log files of one directory, for reading, and the counters the
/// writer and the logging layer share.
#[derive(Clone, Debug)]
pub struct LogFiles {
    directory: PathBuf,
    /// The sequence of the latest record logged.
    latest: Arc<AtomicU64>,
    /// Records the files did not receive.
    dropped: Arc<AtomicU64>,
}

/// Sends records to the background writer of a [`LogFiles`].
#[derive(Debug)]
pub struct LogWriter {
    queue: SyncSender<Message>,
    dropped: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

enum Message {
    Line(String),
    Flush(SyncSender<()>),
}

impl LogFiles {
    /// Opens the log files in `directory`, creating it, and starts their
    /// writer. The sequence continues from the last record the files hold.
    pub fn open(directory: &Path) -> io::Result<(Self, LogWriter)> {
        Self::open_bounded(directory, FILE_MAX_BYTES)
    }

    /// Opens the files as [`open`](Self::open) does, rotating at
    /// `file_max_bytes`.
    fn open_bounded(directory: &Path, file_max_bytes: u64) -> io::Result<(Self, LogWriter)> {
        fs::create_dir_all(directory)?;
        let last = last_sequence(directory)?;
        let current = open_current(directory)?;
        let files = Self {
            directory: directory.to_owned(),
            latest: Arc::new(AtomicU64::new(last)),
            dropped: Arc::new(AtomicU64::new(0)),
        };
        let (queue, received) = sync_channel(QUEUE_RECORDS);
        let writer = Writer {
            directory: directory.to_owned(),
            file_max_bytes,
            dropped: Arc::clone(&files.dropped),
        };
        let thread = std::thread::Builder::new()
            .name("torrplay-logs".into())
            .spawn(move || writer.run(current, &received))?;
        let writer = LogWriter {
            queue,
            dropped: Arc::clone(&files.dropped),
            thread: Some(thread),
        };
        Ok((files, writer))
    }

    /// Assigns the next record's sequence.
    pub fn next_sequence(&self) -> u64 {
        self.latest.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// The sequence of the latest record logged, if any.
    pub fn latest_sequence(&self) -> Option<u64> {
        Some(self.latest.load(Ordering::Relaxed)).filter(|sequence| *sequence > 0)
    }

    /// The records the files did not receive since the process started.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Reads up to `limit` records newest first, after `before` when it is
    /// given and down to, not including, `down_to`, that `keep` accepts, and
    /// the oldest sequence the files hold. It reads files, so callers run it
    /// on a blocking thread.
    pub fn read(
        &self,
        before: Option<u64>,
        down_to: Option<u64>,
        limit: usize,
        keep: impl Fn(&LogRecord) -> bool,
    ) -> io::Result<(Vec<LogRecord>, Option<u64>)> {
        // Every file is opened before any is read, so a rotation meanwhile
        // moves files already open rather than hiding one.
        let mut opened = Vec::new();
        for index in 0..FILES {
            match File::open(self.directory.join(file_name(index))) {
                Ok(file) => opened.push(file),
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        let mut records = Vec::new();
        let mut oldest = None;
        // Records are read newest first; one that is not older than the one
        // before it was read twice across a rotation and is skipped.
        let mut previous = u64::MAX;
        let mut done = false;
        for file in opened {
            let mut lines = Vec::new();
            for line in BufReader::new(file).lines() {
                lines.push(line?);
            }
            for line in lines.iter().rev() {
                // A line the writer has not finished is no record yet.
                let Ok(record) = serde_json::from_str::<LogRecord>(line) else {
                    continue;
                };
                if record.sequence >= previous {
                    continue;
                }
                previous = record.sequence;
                oldest = Some(record.sequence);
                if done || before.is_some_and(|before| record.sequence >= before) || !keep(&record)
                {
                    continue;
                }
                if down_to.is_some_and(|down_to| record.sequence <= down_to) {
                    done = true;
                    continue;
                }
                records.push(record);
                done = records.len() >= limit;
            }
        }
        Ok((records, oldest))
    }
}

impl LogWriter {
    /// Queues a record's line, or drops and counts it when the queue is
    /// full.
    pub fn write(&self, line: String) {
        match self.queue.try_send(Message::Line(line)) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Waits up to `timeout` for the records queued so far to reach the
    /// files. Returns whether they did.
    pub fn flush(&self, timeout: Duration) -> bool {
        let (done, flushed) = sync_channel(1);
        if self.queue.send(Message::Flush(done)).is_err() {
            return false;
        }
        flushed.recv_timeout(timeout).is_ok()
    }

    /// Writes what is queued and stops the writer.
    pub fn close(mut self) -> io::Result<()> {
        let thread = self.thread.take();
        drop(self);
        match thread.map(JoinHandle::join) {
            Some(Err(_)) => Err(io::Error::other("the log writer panicked")),
            _ => Ok(()),
        }
    }
}

struct Writer {
    directory: PathBuf,
    file_max_bytes: u64,
    dropped: Arc<AtomicU64>,
}

impl Writer {
    fn run(&self, mut current: Current, received: &Receiver<Message>) {
        loop {
            let message = match received.recv_timeout(Duration::from_secs(1)) {
                Ok(message) => message,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            self.handle(&mut current, message);
            // A burst is written before the buffer is flushed once.
            while let Ok(message) = received.try_recv() {
                self.handle(&mut current, message);
            }
            if current.file.flush().is_err() {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        let _ = current.file.flush();
    }

    fn handle(&self, current: &mut Current, message: Message) {
        match message {
            Message::Line(line) => {
                let length = line.len() as u64 + 1;
                if current.bytes > 0 && current.bytes + length > self.file_max_bytes {
                    match self.rotate(current) {
                        Ok(rotated) => *current = rotated,
                        Err(_) => {
                            self.dropped.fetch_add(1, Ordering::Relaxed);
                            return;
                        }
                    }
                }
                let written = current
                    .file
                    .write_all(line.as_bytes())
                    .and_then(|()| current.file.write_all(b"\n"));
                match written {
                    Ok(()) => current.bytes += length,
                    Err(_) => {
                        self.dropped.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Message::Flush(done) => {
                if current.file.flush().is_err() {
                    self.dropped.fetch_add(1, Ordering::Relaxed);
                }
                let _ = done.send(());
            }
        }
    }

    /// Moves each file up one number, deleting the oldest, and opens a new
    /// current file.
    fn rotate(&self, current: &mut Current) -> io::Result<Current> {
        current.file.flush()?;
        let path = |index| self.directory.join(file_name(index));
        match fs::remove_file(path(FILES - 1)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for index in (0..FILES - 1).rev() {
            match fs::rename(path(index), path(index + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        open_current(&self.directory)
    }
}

/// The current file, opened for appending, and its length.
struct Current {
    file: BufWriter<File>,
    bytes: u64,
}

fn open_current(directory: &Path) -> io::Result<Current> {
    let path = directory.join(CURRENT);
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    let mut bytes = file.metadata()?.len();
    // A line a crash cut short is ended, so the next record starts a line
    // of its own.
    if bytes > 0 && fs::read(&path)?.last() != Some(&b'\n') {
        file.write_all(b"\n")?;
        bytes += 1;
    }
    Ok(Current {
        file: BufWriter::new(file),
        bytes,
    })
}

/// The sequence of the newest record the files hold, or 0 without one.
fn last_sequence(directory: &Path) -> io::Result<u64> {
    for index in 0..FILES {
        let file = match File::open(directory.join(file_name(index))) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        let mut last = None;
        for line in BufReader::new(file).lines() {
            if let Ok(record) = serde_json::from_str::<LogRecord>(&line?) {
                last = Some(record.sequence);
            }
        }
        if let Some(last) = last {
            return Ok(last);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(sequence: u64, level: &str, message: &str) -> LogRecord {
        LogRecord {
            sequence,
            logged_at: "2026-10-30T09:15:02.481Z".into(),
            level: level.into(),
            run_id: "run_aaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
            request_id: None,
            target: "torrplay_server".into(),
            message: message.into(),
            fields: BTreeMap::new(),
        }
    }

    fn log(files: &LogFiles, writer: &LogWriter, level: &str, message: &str) {
        let line = serde_json::to_string(&record(files.next_sequence(), level, message)).unwrap();
        writer.write(line);
    }

    fn sequences(records: &[LogRecord]) -> Vec<u64> {
        records.iter().map(|record| record.sequence).collect()
    }

    #[test]
    fn records_written_before_the_rename_still_read() {
        let line = r#"{"sequence":7,"time":"2026-10-29T08:00:00.000Z","level":"warn","run_id":"run_aaaaaaaaaaaaaaaaaaaaaaaaaa","request_id":null,"target":"torrplay","message":"old","fields":{}}"#;
        let record: LogRecord = serde_json::from_str(line).unwrap();
        assert_eq!(record.logged_at, "2026-10-29T08:00:00.000Z");
        let written = serde_json::to_string(&record).unwrap();
        assert!(
            written.contains(r#""logged_at":"#),
            "new records use the new name"
        );
    }

    #[test]
    fn records_are_read_newest_first_with_filters_and_bounds() {
        let directory = tempfile::TempDir::new().unwrap();
        let (files, writer) = LogFiles::open(directory.path()).unwrap();
        for (level, message) in [
            ("info", "one"),
            ("warn", "two"),
            ("info", "three"),
            ("error", "four"),
        ] {
            log(&files, &writer, level, message);
        }
        assert!(writer.flush(Duration::from_secs(5)));
        let all = |_: &LogRecord| true;
        let (records, oldest) = files.read(None, None, 10, all).unwrap();
        assert_eq!(sequences(&records), [4, 3, 2, 1]);
        assert_eq!(oldest, Some(1));
        let (page, _) = files.read(None, None, 2, all).unwrap();
        assert_eq!(sequences(&page), [4, 3], "a limit ends the page");
        let (after, _) = files.read(Some(3), None, 10, all).unwrap();
        assert_eq!(sequences(&after), [2, 1], "before a cursor's sequence");
        let (newer, _) = files.read(None, Some(2), 10, all).unwrap();
        assert_eq!(sequences(&newer), [4, 3], "after_sequence bounds it");
        let (warnings, _) = files
            .read(None, None, 10, |record| record.level != "info")
            .unwrap();
        assert_eq!(sequences(&warnings), [4, 2]);
        assert_eq!(files.latest_sequence(), Some(4));
        writer.close().unwrap();
    }

    #[test]
    fn a_reopen_continues_the_sequence() {
        let directory = tempfile::TempDir::new().unwrap();
        {
            let (files, writer) = LogFiles::open(directory.path()).unwrap();
            log(&files, &writer, "info", "before");
            writer.close().unwrap();
        }
        let (files, writer) = LogFiles::open(directory.path()).unwrap();
        assert_eq!(files.latest_sequence(), Some(1));
        log(&files, &writer, "info", "after");
        writer.close().unwrap();
        let (records, _) = files.read(None, None, 10, |_| true).unwrap();
        assert_eq!(sequences(&records), [2, 1]);
    }

    #[test]
    fn rotation_keeps_the_newest_files_within_their_bound() {
        let directory = tempfile::TempDir::new().unwrap();
        let bound = 64 * 1024;
        let (files, writer) = LogFiles::open_bounded(directory.path(), bound).unwrap();
        // Each record takes about 1 KiB, so a file holds about 60.
        let message = "x".repeat(1024);
        let total = 64 * (FILES as u64 + 2);
        for _ in 0..total {
            log(&files, &writer, "info", &message);
        }
        writer.close().unwrap();
        assert_eq!(files.dropped(), 0);
        let names: Vec<_> = (0..FILES + 1)
            .filter(|index| directory.path().join(file_name(*index)).exists())
            .collect();
        assert_eq!(
            names,
            (0..FILES).collect::<Vec<_>>(),
            "the oldest is deleted"
        );
        for index in 0..FILES {
            let length = fs::metadata(directory.path().join(file_name(index)))
                .unwrap()
                .len();
            assert!(length <= bound, "{index}: {length}");
        }
        let (records, oldest) = files.read(None, None, usize::MAX, |_| true).unwrap();
        assert_eq!(records[0].sequence, total, "the newest is kept");
        assert!(oldest.unwrap() > 1, "the oldest records were removed");
        assert_eq!(
            records.len() as u64,
            total - oldest.unwrap() + 1,
            "no gap or repeat within the files"
        );
    }

    #[test]
    fn a_partial_last_line_is_no_record() {
        let directory = tempfile::TempDir::new().unwrap();
        let complete = serde_json::to_string(&record(7, "info", "whole")).unwrap();
        fs::write(
            directory.path().join(CURRENT),
            format!("{complete}\n{{\"sequence\":8,\"ti"),
        )
        .unwrap();
        let (files, writer) = LogFiles::open(directory.path()).unwrap();
        assert_eq!(files.latest_sequence(), Some(7));
        log(&files, &writer, "info", "next");
        writer.close().unwrap();
        let (records, _) = files.read(None, None, 10, |_| true).unwrap();
        assert_eq!(sequences(&records), [8, 7], "the next record is whole");
    }
}
