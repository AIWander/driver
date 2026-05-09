use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub ts: String,
    pub kind: String,
    #[serde(flatten)]
    pub data: Value,
}

pub trait EventSink: Send {
    fn log(&mut self, kind: &str, data: Value) -> Result<()>;
}

pub struct EventWriter {
    file: File,
    #[allow(dead_code)]
    pub run_dir: PathBuf,
}

impl EventWriter {
    pub fn new(run_dir: &Path) -> Result<Self> {
        fs::create_dir_all(run_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(run_dir.join("run.jsonl"))?;
        Ok(EventWriter {
            file,
            run_dir: run_dir.to_path_buf(),
        })
    }
}

impl EventSink for EventWriter {
    fn log(&mut self, kind: &str, data: Value) -> Result<()> {
        let event = Event {
            ts: Utc::now().to_rfc3339(),
            kind: kind.to_string(),
            data,
        };
        let line = serde_json::to_string(&event)?;
        writeln!(self.file, "{}", line)?;
        self.file.flush()?;
        Ok(())
    }
}

pub struct ChannelSink {
    tx: tokio::sync::mpsc::Sender<Event>,
}

impl ChannelSink {
    pub fn new(tx: tokio::sync::mpsc::Sender<Event>) -> Self {
        Self { tx }
    }
}

impl EventSink for ChannelSink {
    fn log(&mut self, kind: &str, data: Value) -> Result<()> {
        let event = Event {
            ts: Utc::now().to_rfc3339(),
            kind: kind.to_string(),
            data,
        };
        let _ = self.tx.try_send(event);
        Ok(())
    }
}

pub struct DualSink<'a> {
    file: EventWriter,
    channel: &'a mut ChannelSink,
}

impl<'a> DualSink<'a> {
    pub fn new(file: EventWriter, channel: &'a mut ChannelSink) -> Self {
        Self { file, channel }
    }
}

impl EventSink for DualSink<'_> {
    fn log(&mut self, kind: &str, data: Value) -> Result<()> {
        self.file.log(kind, data.clone())?;
        self.channel.log(kind, data)
    }
}
