use std::sync::{Arc, Mutex};
use std::collections::VecDeque;
use serde::Serialize;
use tokio::sync::broadcast;
use tracing_subscriber::Layer;
use tracing::{Event, Subscriber};
use tracing_subscriber::field::Visit;
use tracing_subscriber::registry::LookupSpan;
use std::time::SystemTime;

const BUFFER_CAPACITY: usize = 2000;
const CHANNEL_CAPACITY: usize = 1000;
const MAX_MESSAGE_LEN: usize = 4096;

#[derive(Debug, Clone, Serialize)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: String,
    pub target: String,
    pub message: String,
}

pub struct LogBuffer {
    entries: VecDeque<LogEntry>,
    seq: u64,
}

pub struct BroadcastLayer {
    buffer: Arc<Mutex<LogBuffer>>,
    tx: broadcast::Sender<LogEntry>,
}

impl Clone for BroadcastLayer {
    fn clone(&self) -> Self {
        Self {
            buffer: self.buffer.clone(),
            tx: self.tx.clone(),
        }
    }
}

impl BroadcastLayer {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_CAPACITY);
        Self {
            buffer: Arc::new(Mutex::new(LogBuffer {
                entries: VecDeque::with_capacity(BUFFER_CAPACITY),
                seq: 0,
            })),
            tx,
        }
    }

    pub fn buffer(&self) -> Arc<Mutex<LogBuffer>> {
        self.buffer.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<LogEntry> {
        self.tx.subscribe()
    }
}

impl LogBuffer {
    pub fn snapshot(&self) -> Vec<LogEntry> {
        self.entries.iter().cloned().collect()
    }
}

struct StringVisitor {
    message: String,
}

impl StringVisitor {
    fn new() -> Self {
        Self { message: String::new() }
    }
}

impl Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{:?}", value);
        } else {
            if !self.message.is_empty() {
                self.message.push(' ');
            }
            self.message.push_str(&format!("{}={:?}", field.name(), value));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        } else {
            if !self.message.is_empty() {
                self.message.push(' ');
            }
            self.message.push_str(&format!("{}={}", field.name(), value));
        }
    }
}

impl<S> Layer<S> for BroadcastLayer
where
    S: Subscriber,
    S: for<'lookup> LookupSpan<'lookup>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: tracing_subscriber::layer::Context<'_, S>) {
        let mut visitor = StringVisitor::new();
        event.record(&mut visitor);

        let metadata = event.metadata();
        let message = if visitor.message.len() > MAX_MESSAGE_LEN {
            let mut truncated: String = visitor.message.chars().take(MAX_MESSAGE_LEN).collect();
            truncated.push_str("... (truncated)");
            truncated
        } else {
            visitor.message
        };

        let entry = LogEntry {
            timestamp: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
            level: metadata.level().to_string(),
            target: metadata.target().to_string(),
            message,
        };

        if let Ok(mut buf) = self.buffer.lock() {
            if buf.entries.len() >= BUFFER_CAPACITY {
                buf.entries.pop_front();
            }
            buf.entries.push_back(entry.clone());
            buf.seq += 1;
        }

        let _ = self.tx.send(entry);
    }
}
