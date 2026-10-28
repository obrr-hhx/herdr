//! Read-only Claude transcript provider, also exposed through `herdr agent history`.
//! Uses the public pane API and the active process's session metadata; never guesses
//! the newest project file and never sends input to a terminal.
use std::io;
use std::process::Command;

use serde::{Deserialize, Serialize};

pub(crate) const PYTHON_SOURCE: &str = include_str!("../scripts/history/claude_history.py");
pub(crate) const REMOTE_TARGET_ENV: &str = "HERDR_HISTORY_SSH_TARGET";
pub(crate) const REMOTE_SESSION_ENV: &str = "HERDR_HISTORY_SSH_SESSION";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct HistoryRecord {
    pub id: String,
    pub offset: u64,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct HistoryPage {
    pub version: u32,
    pub session_id: String,
    pub terminal_id: String,
    pub revision: String,
    pub unchanged: bool,
    pub records: Vec<HistoryRecord>,
    pub before: u64,
    pub has_older: bool,
}

pub(crate) fn parse_output(bytes: &[u8]) -> io::Result<HistoryPage> {
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other("history reply exceeds 8 MiB"));
    }
    let value: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| io::Error::other("history provider returned invalid JSON"))?;
    if let Some(message) = value.get("error").and_then(|v| v.as_str()) {
        return Err(io::Error::other(message.to_owned()));
    }
    let page: HistoryPage = serde_json::from_value(value)
        .map_err(|_| io::Error::other("history provider returned invalid data"))?;
    if page.version != 1 || page.records.len() > 50_000 {
        return Err(io::Error::other("unsupported history response"));
    }
    Ok(page)
}

pub(crate) fn read_local(
    session: &str,
    pane: &str,
    before: Option<u64>,
    known: &str,
) -> io::Result<HistoryPage> {
    crate::session::validate_name(session).map_err(io::Error::other)?;
    let binary = std::env::current_exe()?;
    let socket = crate::api::socket_path();
    let output = Command::new("python3")
        .args([
            "-c",
            PYTHON_SOURCE,
            session,
            pane,
            &before.map_or_else(|| "latest".into(), |v| v.to_string()),
            known,
        ])
        .arg(binary)
        .arg(socket)
        .output()?;
    parse_output(&output.stdout)
}
