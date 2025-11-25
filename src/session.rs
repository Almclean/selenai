use std::{
    fs::{self, File},
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;

use crate::types::{Message, ToolLogEntry};

pub struct SessionRecorder {
    session_dir: PathBuf,
}

impl SessionRecorder {
    pub fn new(log_root: impl AsRef<Path>, allow_tool_writes: bool) -> Result<Self> {
        let log_root = log_root.as_ref();
        fs::create_dir_all(log_root)
            .with_context(|| format!("failed to create log directory {}", log_root.display()))?;
        let session_dir = create_unique_session_dir(log_root)?;
        fs::create_dir_all(&session_dir).with_context(|| {
            format!(
                "failed to create session directory {}",
                session_dir.display()
            )
        })?;
        write_metadata(&session_dir, allow_tool_writes)?;
        Ok(Self { session_dir })
    }

    pub fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub fn persist(&self, messages: &[Message], tool_logs: &[ToolLogEntry]) -> Result<()> {
        self.write_jsonl("transcript.jsonl", messages)?;
        self.write_jsonl("tool_logs.jsonl", tool_logs)?;
        Ok(())
    }

    fn write_jsonl<T: Serialize>(&self, filename: &str, items: &[T]) -> Result<()> {
        let path = self.session_dir.join(filename);
        let file = File::create(&path)
            .with_context(|| format!("failed to create log file {}", path.display()))?;
        let mut writer = BufWriter::new(file);
        for item in items {
            let json = serde_json::to_string(item)?;
            let redacted = redact_secrets(&json);
            writer.write_all(redacted.as_bytes())?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
        Ok(())
    }
}

static SECRET_REGEX: OnceLock<Vec<Regex>> = OnceLock::new();

fn get_secret_regexes() -> &'static [Regex] {
    SECRET_REGEX.get_or_init(|| {
        vec![
            Regex::new(r"sk-[a-zA-Z0-9-]{20,}").expect("invalid regex"),
        ]
    })
}

fn redact_secrets(text: &str) -> String {
    let mut result = text.to_string();
    for re in get_secret_regexes() {
        result = re.replace_all(&result, "[REDACTED]").to_string();
    }
    result
}

fn create_unique_session_dir(root: &Path) -> Result<PathBuf> {
    let base = generate_session_dir_name();
    let mut candidate = root.join(&base);
    let mut counter = 1;
    while candidate.exists() {
        counter += 1;
        candidate = root.join(format!("{base}-{counter}"));
    }
    Ok(candidate)
}

fn generate_session_dir_name() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0));
    format!("session-{}-{}", now.as_secs(), std::process::id())
}

#[derive(Serialize)]
struct SessionMetadata {
    version: u8,
    started_unix_ms: u128,
    allow_tool_writes: bool,
}

fn write_metadata(path: &Path, allow_tool_writes: bool) -> Result<()> {
    let metadata = SessionMetadata {
        version: 1,
        started_unix_ms: unix_timestamp_ms(),
        allow_tool_writes,
    };
    let data = serde_json::to_vec_pretty(&metadata)?;
    let file = path.join("metadata.json");
    fs::write(&file, data)
        .with_context(|| format!("failed to write metadata {}", file.display()))?;
    Ok(())
}

fn unix_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::from_secs(0))
        .as_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Message, Role, ToolLogEntry, ToolStatus};
    use tempfile::tempdir;

    #[test]
    fn metadata_records_write_policy() -> Result<()> {
        let root = tempdir()?;
        let recorder = SessionRecorder::new(root.path(), true)?;
        let metadata_path = recorder.session_dir().join("metadata.json");
        let contents = fs::read_to_string(metadata_path)?;
        let json: serde_json::Value = serde_json::from_str(&contents)?;
        assert_eq!(json["allow_tool_writes"], serde_json::json!(true));
        Ok(())
    }

    #[test]
    fn persist_writes_transcript_and_tool_logs() -> Result<()> {
        let root = tempdir()?;
        let recorder = SessionRecorder::new(root.path(), false)?;
        let mut entry = ToolLogEntry::new(1, "demo", "testing");
        entry.status = ToolStatus::Success;
        let messages = vec![Message::new(Role::User, "ping")];
        recorder.persist(&messages, &[entry.clone()])?;
        let transcript_path = recorder.session_dir().join("transcript.jsonl");
        let tool_log_path = recorder.session_dir().join("tool_logs.jsonl");
        assert!(transcript_path.exists());
        assert!(tool_log_path.exists());
        let transcript = fs::read_to_string(transcript_path)?;
        assert!(
            transcript.contains("\"role\":\"User\""),
            "transcript should serialize message role"
        );
        let tool_logs = fs::read_to_string(tool_log_path)?;
        assert!(
            tool_logs.contains("\"title\":\"demo\""),
            "tool logs should contain entry"
        );
        Ok(())
    }

    #[test]
    fn redaction_hides_secrets() -> Result<()> {
        let root = tempdir()?;
        let recorder = SessionRecorder::new(root.path(), false)?;
        let secret = "sk-123456789012345678901234";
        let messages = vec![Message::new(Role::User, &format!("My key is {}", secret))];
        recorder.persist(&messages, &[])?;
        
        let transcript_path = recorder.session_dir().join("transcript.jsonl");
        let content = fs::read_to_string(transcript_path)?;
        assert!(!content.contains(secret), "secret should be redacted");
        assert!(content.contains("[REDACTED]"), "redaction placeholder should appear");
        Ok(())
    }
}
