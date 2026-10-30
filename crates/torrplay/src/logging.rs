// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The JSON log format, the log files, and the count of log records by
//! level, as `docs/observability.md` describes them.
//!
//! [`JsonLayer`] writes one JSON object per record, with the record's
//! sequence, time logged, level, run ID, request ID, target, message, and fields,
//! the fields of its spans included, to standard error in the JSON format
//! and to the log files. Messages and values are bounded and redacted again
//! before they are written.

use std::fmt;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use std::collections::BTreeMap;

use chrono::{SecondsFormat, Utc};
use torrplay_core::id::RunId;
use torrplay_core::redact::redact;
use torrplay_core::settings::LogLevel;
use torrplay_core::telemetry::LogRecordCounts;
use torrplay_server::log_files::{LogFiles, LogRecord, LogWriter};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

/// The most bytes of a message a record keeps.
const MESSAGE_MAX_BYTES: usize = 4096;

/// The most bytes of a field's value a record keeps.
const VALUE_MAX_BYTES: usize = 1024;

/// The most fields a record keeps.
const FIELD_LIMIT: usize = 32;

/// The field that names a record's request, which the record carries as a
/// member of its own rather than among its fields.
const REQUEST_ID_FIELD: &str = "request_id";

/// The level a `tracing` level is in the log format.
fn level(level: &Level) -> LogLevel {
    match *level {
        Level::ERROR => LogLevel::Error,
        Level::WARN => LogLevel::Warn,
        Level::INFO => LogLevel::Info,
        Level::DEBUG => LogLevel::Debug,
        Level::TRACE => LogLevel::Trace,
    }
}

/// Counts the records the filter lets through, by level.
pub(crate) struct CountingLayer(pub(crate) Arc<LogRecordCounts>);

impl<S: Subscriber> Layer<S> for CountingLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        self.0.record(level(event.metadata().level()));
    }
}

/// Writes each record as one line of JSON to the writer `W` makes, if any,
/// and to the log files, if they are open.
pub(crate) struct JsonLayer<W> {
    run_id: String,
    /// Numbers records when no log files do.
    sequence: AtomicU64,
    writer: Option<W>,
    files: Option<(LogFiles, Arc<LogWriter>)>,
}

impl<W> JsonLayer<W> {
    pub(crate) fn new(
        run_id: &RunId,
        writer: Option<W>,
        files: Option<(LogFiles, Arc<LogWriter>)>,
    ) -> Self {
        Self {
            run_id: run_id.to_string(),
            sequence: AtomicU64::new(0),
            writer,
            files,
        }
    }

    /// Assigns the next record's sequence, which continues the log files'.
    fn next_sequence(&self) -> u64 {
        match &self.files {
            Some((files, _)) => files.next_sequence(),
            None => self.sequence.fetch_add(1, Ordering::Relaxed) + 1,
        }
    }
}

/// A span's fields, kept in its extensions for the records logged in it.
#[derive(Default)]
struct SpanFields(Vec<(&'static str, String)>);

/// Collects fields as text, the message apart.
#[derive(Default)]
struct Fields {
    message: Option<String>,
    values: Vec<(&'static str, String)>,
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.push(field, value.to_owned());
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        self.push(field, format!("{value:?}"));
    }
}

impl Fields {
    fn push(&mut self, field: &Field, value: String) {
        if field.name() == "message" {
            self.message = Some(value);
        } else {
            self.values.retain(|(name, _)| *name != field.name());
            self.values.push((field.name(), value));
        }
    }
}

impl<S, W> Layer<S> for JsonLayer<W>
where
    S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    W: for<'writer> MakeWriter<'writer> + 'static,
{
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        if let Some(span) = context.span(id) {
            span.extensions_mut().insert(SpanFields(fields.values));
        }
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, context: Context<'_, S>) {
        let Some(span) = context.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(SpanFields(existing)) = extensions.get_mut::<SpanFields>() else {
            return;
        };
        let mut fields = Fields {
            message: None,
            values: std::mem::take(existing),
        };
        values.record(&mut fields);
        *existing = fields.values;
    }

    fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
        // Spans from the outermost in, then the record's own fields, so an
        // inner value wins over an outer one of the same name.
        let mut fields = Fields::default();
        if let Some(scope) = context.event_scope(event) {
            for span in scope.from_root() {
                if let Some(SpanFields(values)) = span.extensions().get::<SpanFields>() {
                    for (name, value) in values {
                        fields.values.retain(|(existing, _)| existing != name);
                        fields.values.push((name, value.clone()));
                    }
                }
            }
        }
        event.record(&mut fields);
        let metadata = event.metadata();
        let record = self.record(
            level(metadata.level()),
            metadata.target(),
            fields,
            Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        );
        let Ok(line) = serde_json::to_string(&record) else {
            return;
        };
        if let Some(writer) = &self.writer {
            // A record that cannot be written is lost; logging must not fail
            // the code that logs.
            let _ = writer
                .make_writer()
                .write_all(format!("{line}\n").as_bytes());
        }
        if let Some((_, files)) = &self.files {
            files.write(line);
        }
    }
}

impl<W> JsonLayer<W> {
    fn record(
        &self,
        level: LogLevel,
        target: &str,
        fields: Fields,
        logged_at: String,
    ) -> LogRecord {
        let Fields { message, values } = fields;
        let mut request_id = None;
        let mut members = BTreeMap::new();
        for (name, value) in values {
            if name == REQUEST_ID_FIELD {
                request_id = Some(bounded(&value, VALUE_MAX_BYTES));
            } else if members.len() < FIELD_LIMIT {
                members.insert(name.to_owned(), bounded(&value, VALUE_MAX_BYTES));
            }
        }
        LogRecord {
            sequence: self.next_sequence(),
            logged_at,
            level: level.as_str().to_owned(),
            run_id: self.run_id.clone(),
            request_id,
            target: target.to_owned(),
            message: bounded(&message.unwrap_or_default(), MESSAGE_MAX_BYTES),
            fields: members,
        }
    }
}

/// `text` redacted, then cut to `max_bytes` at a character boundary with an
/// ellipsis when it is longer.
fn bounded(text: &str, max_bytes: usize) -> String {
    let mut text = redact(text);
    if text.len() > max_bytes {
        let mut end = max_bytes;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::Value;
    use tracing_subscriber::layer::SubscriberExt;

    use super::*;

    /// Collects what the layer writes.
    #[derive(Clone, Default)]
    struct Lines(Arc<Mutex<Vec<u8>>>);

    impl Write for Lines {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for Lines {
        type Writer = Self;

        fn make_writer(&'writer self) -> Self {
            self.clone()
        }
    }

    fn logged(log: impl FnOnce()) -> Vec<Value> {
        let lines = Lines::default();
        let counts = Arc::new(LogRecordCounts::default());
        let run = RunId::from_random_bytes([7; 16]);
        let subscriber = tracing_subscriber::registry()
            .with(JsonLayer::new(&run, Some(lines.clone()), None))
            .with(CountingLayer(Arc::clone(&counts)));
        tracing::subscriber::with_default(subscriber, log);
        let text = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
        text.lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn records_carry_their_run_request_and_span_fields() {
        let records = logged(|| {
            let request =
                tracing::error_span!("request", request_id = "req_abc", torrent = "outer");
            let _entered = request.enter();
            tracing::warn!(torrent = "trr_x", piece = 12, "a piece {} late", 3);
        });
        let [record] = records.as_slice() else {
            panic!("one record: {records:?}");
        };
        assert_eq!(record["sequence"], 1);
        assert_eq!(record["level"], "warn");
        assert_eq!(record["run_id"], RunId::from_random_bytes([7; 16]).as_str());
        assert_eq!(record["request_id"], "req_abc");
        assert_eq!(record["message"], "a piece 3 late");
        assert_eq!(
            record["fields"],
            serde_json::json!({ "torrent": "trr_x", "piece": "12" }),
            "the record's field wins over the span's"
        );
        assert!(record["logged_at"].as_str().unwrap().ends_with('Z'));
        assert!(record["target"].as_str().unwrap().starts_with("torrplay"));
    }

    #[test]
    fn records_outside_a_request_have_none_and_sequences_count_up() {
        let records = logged(|| {
            tracing::info!("first");
            tracing::info!("second");
        });
        assert_eq!(records[0]["request_id"], Value::Null);
        assert_eq!(
            records
                .iter()
                .map(|record| &record["sequence"])
                .collect::<Vec<_>>(),
            [1, 2]
        );
    }

    #[test]
    fn messages_and_values_are_redacted_and_bounded() {
        let long = "é".repeat(VALUE_MAX_BYTES);
        let records = logged(|| {
            tracing::error!(
                long = %long,
                url = "http://tracker.test/s3cr3t/announce",
                "announce to http://tracker.test/s3cr3t/announce failed"
            );
        });
        let record = &records[0];
        assert!(!record.to_string().contains("s3cr3t"), "{record}");
        let value = record["fields"]["long"].as_str().unwrap();
        assert!(value.ends_with('…'));
        assert!(value.len() <= VALUE_MAX_BYTES + '…'.len_utf8());
    }

    #[test]
    fn records_keep_at_most_the_field_limit() {
        let records = logged(|| {
            let span = tracing::info_span!(
                "many",
                a0 = 0,
                a1 = 1,
                a2 = 2,
                a3 = 3,
                a4 = 4,
                a5 = 5,
                a6 = 6,
                a7 = 7,
                a8 = 8,
                a9 = 9,
                b0 = 0,
                b1 = 1,
                b2 = 2,
                b3 = 3,
                b4 = 4,
                b5 = 5,
                b6 = 6,
                b7 = 7,
                b8 = 8,
                b9 = 9,
                c0 = 0,
                c1 = 1,
                c2 = 2,
                c3 = 3,
                c4 = 4,
                c5 = 5,
                c6 = 6,
                c7 = 7,
                c8 = 8,
                c9 = 9
            );
            let _entered = span.enter();
            tracing::info!(d0 = 0, d1 = 1, d2 = 2, "many fields");
        });
        assert_eq!(records[0]["fields"].as_object().unwrap().len(), FIELD_LIMIT);
    }
}
