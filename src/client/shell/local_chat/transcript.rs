//! Structured conversation history; screen snapshots never replace saved records.
use super::*;
use crate::agent_history::{HistoryPage, HistoryRecord};
use crate::client::history_jobs::HistoryResult;

const MAX_RECORDS: usize = 10_000;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::client::shell) enum LineKind {
    #[default]
    Plain,
    User,
    Assistant,
    Tool,
    Code,
    Heading,
    Muted,
    Error,
}

#[derive(Default)]
pub(in crate::client::shell) struct Transcript {
    pub(in crate::client::shell) loaded: bool,
    pub(in crate::client::shell) busy: bool,
    pub(in crate::client::shell) revision: String,
    pub(in crate::client::shell) session: String,
    terminal: String,
    pub(in crate::client::shell) before: u64,
    pub(in crate::client::shell) has_older: bool,
    pub(in crate::client::shell) want_older: bool,
    pub(in crate::client::shell) next: Option<Instant>,
    pub(in crate::client::shell) records: Vec<HistoryRecord>,
}

pub(in crate::client::shell) struct Document {
    pub(in crate::client::shell) lines: Vec<(String, LineKind)>,
    pub(in crate::client::shell) anchors: Vec<(String, usize)>,
}

pub(in crate::client::shell) fn document(records: &[HistoryRecord]) -> Document {
    let mut lines = Vec::new();
    let mut anchors = Vec::new();
    let mut bytes = 0;
    for record in records {
        let (label, kind) = match record.kind.as_str() {
            "user" => ("YOU", LineKind::User),
            "assistant" => ("CLAUDE", LineKind::Assistant),
            "tool_call" => ("TOOL", LineKind::Tool),
            "tool_result" => ("TOOL RESULT", LineKind::Tool),
            "error" => ("TOOL ERROR", LineKind::Error),
            _ => continue,
        };
        anchors.push((record.id.clone(), bytes));
        bytes += label.len() + 1;
        lines.push((
            label.to_string(),
            if kind == LineKind::Assistant {
                LineKind::Heading
            } else {
                kind
            },
        ));
        let mut code = false;
        for line in record.text.lines() {
            let style = if line.trim_start().starts_with("```") {
                code = !code;
                LineKind::Muted
            } else if code {
                LineKind::Code
            } else if line.starts_with('#') {
                LineKind::Heading
            } else {
                kind
            };
            bytes += line.len() + 1;
            lines.push((line.to_owned(), style));
        }
        bytes += 1;
        lines.push((String::new(), LineKind::Plain));
    }
    Document { lines, anchors }
}

impl LocalChat {
    pub(in crate::client::shell) fn rewrap_history(&mut self) {
        self.wrapped_styles.clear();
        if self.document_lines.is_empty() {
            self.wrapped = wrap(&self.text, self.width);
            return;
        }
        self.wrapped.clear();
        for (line, kind) in &self.document_lines {
            let rows = wrap(line, self.width);
            self.wrapped_styles
                .extend(std::iter::repeat_n(*kind, rows.len()));
            self.wrapped.extend(rows);
        }
    }

    fn install_document(&mut self, document: Document) {
        let lines = document.lines;
        self.document_anchors = document.anchors;
        self.text = lines
            .iter()
            .map(|(line, _)| line.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        self.document_lines = lines;
        self.rewrap_history();
    }

    pub(in crate::client::shell) fn resume_transcript(&mut self) {
        if let Some(lines) = self.pending_document.take() {
            self.install_document(lines);
        }
    }

    pub(in crate::client::shell) fn request_older(&mut self) {
        if self.transcript.loaded
            && self.transcript.has_older
            && self.offset == self.wrapped.len().saturating_sub(self.viewport_rows)
        {
            self.transcript.want_older = true;
            self.transcript.next = None;
        }
    }

    fn apply_transcript(&mut self, page: HistoryPage, older: bool) -> bool {
        if page.unchanged {
            self.transcript.revision = page.revision;
            return false;
        }
        let changed_session = self.transcript.loaded
            && (self.transcript.session != page.session_id
                || self.transcript.terminal != page.terminal_id);
        if changed_session {
            self.transcript.records.clear();
            self.pending_document = None;
            self.offset = 0;
            if !self.draft.is_empty() {
                self.uncertain = true;
                self.status =
                    "Claude session changed. Esc: inspect; Ctrl+R: allow sending this draft."
                        .into();
            }
        }
        let first = !self.transcript.loaded || changed_session;
        let metadata_changed = first
            || (older
                && (self.transcript.before != page.before
                    || self.transcript.has_older != page.has_older
                    || self.transcript.want_older));
        self.transcript.session = page.session_id;
        self.transcript.terminal = page.terminal_id;
        if !older {
            self.transcript.revision = page.revision;
        } else if first {
            self.transcript.revision.clear(); // An older-page response cannot establish a new session's latest tail.
        }
        if first || older {
            self.transcript.before = page.before;
            self.transcript.has_older = page.has_older;
        }
        self.transcript.loaded = true;
        if older || first {
            self.transcript.want_older = false;
        }
        let mut positions = self
            .transcript
            .records
            .iter()
            .enumerate()
            .map(|(i, r)| (r.id.clone(), i))
            .collect::<std::collections::HashMap<_, _>>();
        let mut changed = first;
        for record in page.records {
            if let Some(&index) = positions.get(&record.id) {
                let previous = &self.transcript.records[index];
                if previous.text != record.text || previous.kind != record.kind {
                    self.transcript.records[index] = record;
                    changed = true;
                }
            } else {
                positions.insert(record.id.clone(), self.transcript.records.len());
                self.transcript.records.push(record);
                changed = true;
            }
        }
        if !changed {
            return metadata_changed;
        }
        self.transcript
            .records
            .sort_by(|a, b| a.offset.cmp(&b.offset).then(a.id.cmp(&b.id)));
        let mut bytes: usize = self.transcript.records.iter().map(|r| r.text.len()).sum();
        let mut evicted_front = false;
        while self.transcript.records.len() > MAX_RECORDS || bytes > MAX_BYTES {
            let removed = if older || self.offset > 0 {
                self.transcript.records.pop()
            } else {
                evicted_front = true;
                Some(self.transcript.records.remove(0))
            };
            if let Some(record) = removed {
                bytes = bytes.saturating_sub(record.text.len());
            } else {
                break;
            }
            self.truncated = true;
        }
        if evicted_front
            && self
                .transcript
                .records
                .first()
                .is_some_and(|r| r.offset > self.transcript.before)
        {
            self.transcript.before = self.transcript.records[0].offset;
            self.transcript.has_older = self.transcript.before > 0;
        }
        let lines = document(&self.transcript.records);
        if self.offset > 0 && !older {
            self.pending_document = Some(lines);
        } else if self.offset > 0 && older {
            let anchor = self
                .wrapped
                .len()
                .saturating_sub(self.viewport_rows)
                .saturating_sub(self.offset);
            let anchor_byte = wrap_starts(&self.text, self.width)
                .get(anchor)
                .copied()
                .unwrap_or(0);
            let old_anchor = self
                .document_anchors
                .iter()
                .rev()
                .find(|(_, start)| *start <= anchor_byte)
                .map(|(id, start)| (id.clone(), anchor_byte - *start));
            self.install_document(lines);
            self.pending_document = None; // This document already incorporates pending saved records.
            let anchor = old_anchor
                .and_then(|(id, relative)| {
                    self.document_anchors
                        .iter()
                        .position(|(other, _)| *other == id)
                        .map(|index| {
                            let start = self.document_anchors[index].1;
                            let end = self
                                .document_anchors
                                .get(index + 1)
                                .map_or(self.text.len(), |(_, position)| *position);
                            start + relative.min(end.saturating_sub(start + 1))
                        })
                })
                .unwrap_or(0);
            let starts = wrap_starts(&self.text, self.width);
            let top = starts.partition_point(|p| *p <= anchor).saturating_sub(1);
            self.offset = self
                .wrapped
                .len()
                .saturating_sub(self.viewport_rows)
                .saturating_sub(top);
        } else {
            self.install_document(lines);
            self.pending_history = None;
        }
        true
    }
}

impl ClientShellState {
    pub(crate) fn complete_agent_history(&mut self, completed: HistoryResult) -> bool {
        let Some(chat) = self.local_chat.as_mut().filter(|c| {
            c.endpoint == completed.endpoint
                && c.boot_id == completed.boot
                && c.generation == completed.generation
        }) else {
            return false;
        };
        chat.transcript.busy = false;
        if self.active_endpoint_id != completed.endpoint
            || self
                .snapshot
                .as_deref()
                .is_none_or(|s| s.boot_id != completed.boot)
        {
            return false;
        }
        match completed.result {
            Ok(page) => {
                let status_changed = !chat.uncertain
                    && (chat.status.starts_with("Saved history unavailable")
                        || chat.status.starts_with("History unavailable")
                        || chat.status.starts_with("Unexpected history"));
                if status_changed {
                    chat.status = "Saved conversation history loaded".into();
                }
                let changed = chat.apply_transcript(page, completed.older);
                chat.active && (changed || status_changed)
            }
            Err(error) => {
                chat.transcript.next = Some(Instant::now() + Duration::from_secs(5));
                chat.status =
                    format!("Saved history unavailable: {error}. Esc: original terminal.");
                chat.active
            }
        }
    }
}

pub(in crate::client::shell) fn line_style(kind: LineKind, palette: &Palette) -> Style {
    let base = Style::default().bg(palette.panel_bg);
    match kind {
        LineKind::Plain | LineKind::Assistant => base.fg(palette.text),
        LineKind::User => base.fg(palette.green),
        LineKind::Tool => base.fg(palette.mauve),
        LineKind::Code => base.fg(palette.teal).bg(palette.sidebar_bg),
        LineKind::Heading => base.fg(palette.accent).add_modifier(Modifier::BOLD),
        LineKind::Muted => base.fg(palette.overlay0),
        LineKind::Error => base.fg(palette.red),
    }
}
