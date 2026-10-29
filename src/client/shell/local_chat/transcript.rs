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

// Display Markdown emphasis without leaking markup into the terminal transcript.
// Code spans keep their literal contents, including any emphasis delimiters.
fn markdown_text(mut text: &str) -> String {
    let mut rendered = String::new();
    while !text.is_empty() {
        let next = ["`", "**", "__"]
            .into_iter()
            .filter_map(|marker| text.find(marker).map(|offset| (offset, marker)))
            .min_by_key(|(offset, _)| *offset);
        let Some((offset, marker)) = next else {
            rendered.push_str(text);
            break;
        };
        rendered.push_str(&text[..offset]);
        let rest = &text[offset + marker.len()..];
        if let Some(end) = rest.find(marker).filter(|end| *end > 0) {
            let body = &rest[..end];
            if marker == "`" {
                rendered.push_str(body);
            } else {
                rendered.push_str(&markdown_text(body));
            }
            text = &rest[end + marker.len()..];
        } else {
            rendered.push_str(marker);
            text = rest;
        }
    }
    rendered
}

pub(in crate::client::shell) fn document(records: &[HistoryRecord]) -> Document {
    let mut lines = Vec::new();
    let mut anchors = Vec::new();
    let mut bytes = 0;
    for record in records {
        let (prefix, kind) = match record.kind.as_str() {
            "user" => ("❯ ", LineKind::User),
            "assistant" => ("⏺ ", LineKind::Assistant),
            "tool_call" => ("⏺ ", LineKind::Tool),
            "tool_result" => ("  ⎿  ", LineKind::Muted),
            "error" => ("  ⎿  ", LineKind::Error),
            _ => continue,
        };
        anchors.push((record.id.clone(), bytes));
        let text = if kind == LineKind::User {
            echo::saved_prompt_text(&record.text)
        } else {
            &record.text
        };
        let mut code = false;
        let mut first = true;
        for line in text.lines() {
            if kind == LineKind::Assistant && line.trim_start().starts_with("```") {
                code = !code;
                continue;
            }
            let heading = kind == LineKind::Assistant && !code && line.starts_with('#');
            let line = if heading {
                line.trim_start_matches('#').trim_start()
            } else {
                line
            };
            let style = if code {
                LineKind::Code
            } else if heading {
                LineKind::Heading
            } else {
                kind
            };
            let prefix = if first {
                prefix
            } else if matches!(record.kind.as_str(), "tool_result" | "error") {
                "     "
            } else {
                "  "
            };
            let rendered = if kind == LineKind::Assistant && !code {
                format!("{prefix}{}", markdown_text(line))
            } else {
                format!("{prefix}{line}")
            };
            bytes += rendered.len() + 1;
            lines.push((rendered, style));
            first = false;
        }
        if record.kind != "tool_call" {
            bytes += 1;
            lines.push((String::new(), LineKind::Plain));
        }
    }
    Document { lines, anchors }
}

fn native_wrapped_line(line: &str, width: u16) -> (Vec<String>, Vec<usize>) {
    let prefix = ["  ⎿  ", "❯ ", "⏺ ", "     ", "  "]
        .into_iter()
        .find(|prefix| line.starts_with(*prefix))
        .unwrap_or("");
    let indent = UnicodeWidthStr::width(prefix);
    let body = &line[prefix.len()..];
    let body_width = width.saturating_sub(indent as u16).max(1);
    let rows = wrap(body, body_width);
    let starts = wrap_starts(body, body_width)
        .into_iter()
        .enumerate()
        .map(|(index, start)| if index == 0 { 0 } else { prefix.len() + start })
        .collect();
    let rows = rows
        .into_iter()
        .enumerate()
        .map(|(index, row)| {
            format!(
                "{}{row}",
                if index == 0 {
                    prefix.to_owned()
                } else {
                    " ".repeat(indent)
                }
            )
        })
        .collect();
    (rows, starts)
}

impl LocalChat {
    pub(in crate::client::shell) fn rewrap_history(&mut self) {
        self.wrapped_styles.clear();
        self.history_row_starts.clear();
        if self.document_lines.is_empty() {
            self.wrapped = wrap(&self.text, self.width);
            self.history_row_starts = wrap_starts(&self.text, self.width);
            return;
        }
        self.wrapped.clear();
        let mut bytes = 0;
        for (line, kind) in &self.document_lines {
            let (rows, starts) = native_wrapped_line(line, self.width);
            self.wrapped_styles
                .extend(std::iter::repeat_n(*kind, rows.len()));
            self.history_row_starts
                .extend(starts.into_iter().map(|start| bytes + start));
            self.wrapped.extend(rows);
            bytes += line.len() + 1;
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
        let rows = if self.live_surface.body.is_some() {
            self.live_surface.row_count()
        } else {
            self.wrapped.len()
        };
        if self.transcript.loaded
            && self.transcript.has_older
            && self.offset >= rows.saturating_sub(self.viewport_rows.saturating_mul(2))
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
        // PTY handoff replaces runtime IDs without replacing the Claude conversation.
        // Endpoint boot/generation already fences the old transport independently.
        let changed_session = self.transcript.loaded && self.transcript.session != page.session_id;
        if changed_session {
            self.live_surface = LiveSurface::default();
            self.last_revision = None;
            if let Some(sent) = self.sending.take() {
                self.draft = format!(
                    "{sent}{}{}",
                    if self.draft.is_empty() { "" } else { "\n" },
                    self.draft
                );
                self.cursor = self.draft.len();
                self.draft_dirty = true;
            }
            self.echoes.clear();
            self.echo_dirty = true;
            self.transcript.records.clear();
            self.pending_document = None;
            self.offset = 0;
            self.reading_history = false;
            if !self.draft.is_empty() {
                self.uncertain = true;
                self.status =
                    "Claude session changed. Esc: inspect; Ctrl+R: allow sending this draft."
                        .into();
            }
        }
        let first = !self.transcript.loaded || changed_session;
        self.confirm_echoes(&page.records);
        let metadata_changed = first
            || (older
                && (self.transcript.before != page.before
                    || self.transcript.has_older != page.has_older
                    || self.transcript.want_older));
        self.transcript.session = page.session_id;
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
        if self.reading_history
            && self.live_surface.body.is_some()
            && (older || self.live_surface.awaiting_saved)
        {
            self.install_document(lines);
            self.pending_document = None;
            self.live_surface.reanchor(&self.wrapped);
            if !self.live_surface.awaiting_saved {
                self.offset = self
                    .offset
                    .saturating_add(std::mem::take(&mut self.live_surface.pending_scroll))
                    .min(
                        self.live_surface
                            .row_count()
                            .saturating_sub(self.viewport_rows),
                    );
            }
        } else if (self.offset > 0 || self.reading_history) && !older {
            self.pending_document = Some(lines);
        } else if self.offset > 0 && older {
            let anchor = self
                .wrapped
                .len()
                .saturating_sub(self.viewport_rows)
                .saturating_sub(self.offset);
            let anchor_byte = self.history_row_starts.get(anchor).copied().unwrap_or(0);
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
            let top = self
                .history_row_starts
                .partition_point(|p| *p <= anchor)
                .saturating_sub(1);
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
                let previous_session = chat
                    .transcript
                    .loaded
                    .then(|| chat.transcript.session.clone());
                let changed = chat.apply_transcript(page, completed.older);
                tracing::debug!(target:"herdr::client::diagnostics", records=chat.transcript.records.len(), rows=chat.wrapped.len(), offset=chat.offset, older=completed.older, "local history cache refreshed");
                if previous_session.is_some_and(|session| session != chat.transcript.session) {
                    self.local_chat_generation = self.local_chat_generation.saturating_add(1);
                    chat.generation = self.local_chat_generation;
                }
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
        LineKind::User => base.fg(palette.panel_bg).bg(palette.text),
        LineKind::Tool => base.fg(palette.mauve),
        LineKind::Code => base.fg(palette.teal).bg(palette.sidebar_bg),
        LineKind::Heading => base.fg(palette.text).add_modifier(Modifier::BOLD),
        LineKind::Muted => base.fg(palette.overlay0),
        LineKind::Error => base.fg(palette.red),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_markdown_keeps_literal_code_and_unclosed_delimiters() {
        assert_eq!(markdown_text("**bold** and `2**3`"), "bold and 2**3");
        assert_eq!(
            markdown_text("unclosed ** and `literal"),
            "unclosed ** and `literal"
        );
    }

    #[test]
    fn native_wrapping_retains_hanging_indent_and_unicode_byte_anchors() {
        let line = "⏺ 中文 Unicode content that wraps across multiple rows";
        let (rows, starts) = native_wrapped_line(line, 12);
        assert_eq!(rows.len(), starts.len());
        assert!(rows[0].starts_with("⏺ "));
        assert!(rows[1..].iter().all(|row| row.starts_with("  ")));
        assert!(rows
            .iter()
            .all(|row| UnicodeWidthStr::width(row.as_str()) <= 12));
        assert!(starts.iter().all(|start| line.is_char_boundary(*start)));
        assert!(starts.windows(2).all(|pair| pair[0] < pair[1]));
    }
}
