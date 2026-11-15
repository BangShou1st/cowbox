use crate::error::{CliError, CliResult};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::field::{Field, Visit};
use tracing::span::Id;
use tracing::{Event, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone)]
pub struct ChromeLayer {
    start: Instant,
    events: Arc<Mutex<Vec<Value>>>,
}

impl ChromeLayer {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn finish(&self, path: &Path) -> CliResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let events = self
            .events
            .lock()
            .map_err(|_| CliError::new("TRACE_ERROR", "trace lock poisoned"))?
            .clone();
        let data = serde_json::to_vec_pretty(&json!({ "traceEvents": events }))?;
        std::fs::write(path, data)?;
        Ok(())
    }

    fn timestamp(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }

    fn push(&self, event: Value) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }
}

impl Default for ChromeLayer {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Layer<S> for ChromeLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let name = ctx
            .span(id)
            .map(|span| span.name().to_string())
            .unwrap_or_else(|| "span".to_string());
        self.push(json!({
            "ph": "B",
            "name": name,
            "ts": self.timestamp(),
            "pid": std::process::id(),
            "tid": 0,
        }));
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let name = ctx
            .span(id)
            .map(|span| span.name().to_string())
            .unwrap_or_else(|| "span".to_string());
        self.push(json!({
            "ph": "E",
            "name": name,
            "ts": self.timestamp(),
            "pid": std::process::id(),
            "tid": 0,
        }));
    }

    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let mut visitor = MessageVisitor::default();
        event.record(&mut visitor);
        self.push(json!({
            "ph": "i",
            "name": "log",
            "ts": self.timestamp(),
            "pid": std::process::id(),
            "tid": 0,
            "args": { "message": visitor.message },
        }));
    }
}

#[derive(Default)]
struct MessageVisitor {
    message: String,
}

impl Visit for MessageVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.message = value.to_string();
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        }
    }
}

pub fn init(trace_path: Option<&Path>) -> CliResult<Option<ChromeLayer>> {
    use tracing_subscriber::prelude::*;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("cowbox=info"));
    let fmt_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

    if let Some(_path) = trace_path {
        let chrome = ChromeLayer::new();
        let subscriber = tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .with(chrome.clone());
        subscriber
            .try_init()
            .map_err(|error| CliError::new("TRACE_ERROR", error.to_string()))?;
        Ok(Some(chrome))
    } else {
        tracing_subscriber::registry()
            .with(filter)
            .with(fmt_layer)
            .try_init()
            .map_err(|error| CliError::new("TRACE_ERROR", error.to_string()))?;
        Ok(None)
    }
}
