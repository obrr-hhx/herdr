//! Client-owned draft and bounded reader. Nothing here predicts remote terminal state.
use super::*;
use crate::api::schema::{
    AgentPromptParams, Method, PaneReadParams, ReadFormat, ReadSource, ResponseResult,
};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

const MAX_DRAFT_BYTES: usize = 64 * 1024;
const MAX_CACHE_BYTES: usize = 1024 * 1024;
const READ_LINES: u32 = 2000;
const REFRESH: Duration = Duration::from_millis(500);

pub(super) struct LocalChat {
    pub(super) active: bool,
    endpoint: ClientEndpointId,
    boot_id: String,
    pane_id: String,
    generation: u64,
    pub(super) draft: String,
    cursor: usize,
    sending: Option<String>,
    uncertain: bool,
    pub(super) text: String,
    wrapped: Vec<String>,
    pending_history: Option<(String, bool)>,
    draft_layout: Vec<String>,
    draft_prefix: Vec<String>,
    draft_dirty: bool,
    width: u16,
    pub(super) offset: usize,
    viewport_rows: usize,
    pub(super) status: String,
    truncated: bool,
    in_flight: bool,
    last_revision: Option<u64>,
    next_refresh: Instant,
    area: Rect,
}

fn wrap(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut lines = vec![String::new()];
    let mut column = 0;
    for ch in text.chars() {
        if ch == '\n' {
            lines.push(String::new());
            column = 0;
        } else if !ch.is_control() || ch == '\t' {
            let ch = if ch == '\t' { ' ' } else { ch };
            let cells = ch.width().unwrap_or(0);
            if column + cells > width && column > 0 {
                lines.push(String::new());
                column = 0;
            }
            if let Some(line) = lines.last_mut() {
                line.push(ch);
            }
            column += cells;
        }
    }
    lines
}

impl LocalChat {
    fn insert(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        // Input pasted into a draft must never become terminal control sequences.
        let text: String = text
            .chars()
            .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
            .collect();
        if self.draft.len().saturating_add(text.len()) > MAX_DRAFT_BYTES {
            self.status = "Draft limit: 64 KiB. Text was not inserted.".into();
            return;
        }
        self.draft_dirty = true;
        self.draft.insert_str(self.cursor, &text);
        self.cursor += text.len();
    }

    fn previous(&self) -> usize {
        self.draft[..self.cursor]
            .char_indices()
            .next_back()
            .map_or(0, |(i, _)| i)
    }

    fn next(&self) -> usize {
        self.draft[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |c| self.cursor + c.len_utf8())
    }

    fn scroll(&mut self, delta: isize) {
        self.offset = self
            .offset
            .saturating_add_signed(delta)
            .min(self.wrapped.len().saturating_sub(self.viewport_rows));
        if self.offset == 0 {
            self.resume_live();
        }
    }

    fn update_text(&mut self, mut text: String, truncated: bool) {
        let mut cut = text.len().saturating_sub(MAX_CACHE_BYTES);
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        if cut > 0 {
            text.drain(..cut);
        }
        let truncated = truncated || cut > 0;
        if self.offset > 0 {
            self.pending_history = (self.text != text).then_some((text, truncated));
            return;
        }
        self.truncated = truncated;
        if self.text == text {
            return;
        }
        self.text = text;
        self.wrapped = wrap(&self.text, self.width);
    }

    fn resume_live(&mut self) {
        self.offset = 0;
        if let Some((text, truncated)) = self.pending_history.take() {
            self.update_text(text, truncated);
        }
    }

    fn move_vertical(&mut self, up: bool) {
        let start = self.draft[..self.cursor].rfind('\n').map_or(0, |p| p + 1);
        let column = self.draft[start..self.cursor].chars().count();
        let (target, end) = if up {
            if start == 0 {
                return;
            }
            let end = start - 1;
            (self.draft[..end].rfind('\n').map_or(0, |p| p + 1), end)
        } else {
            let Some(end) = self.draft[self.cursor..]
                .find('\n')
                .map(|p| self.cursor + p)
            else {
                return;
            };
            let target = end + 1;
            (
                target,
                self.draft[target..]
                    .find('\n')
                    .map_or(self.draft.len(), |p| target + p),
            )
        };
        self.cursor = self.draft[target..end]
            .char_indices()
            .nth(column)
            .map_or(end, |(p, _)| target + p);
    }
}

impl ClientShellState {
    pub(super) fn local_chat_visible(&self) -> bool {
        self.overlay.is_none()
            && self.local_chat_matches()
            && self.local_chat.as_ref().is_some_and(|c| c.active)
    }

    fn local_chat_matches(&self) -> bool {
        self.local_chat.as_ref().is_some_and(|chat| {
            chat.endpoint == self.active_endpoint_id
                && self
                    .snapshot
                    .as_deref()
                    .is_some_and(|s| s.focused_pane_id.as_ref() == Some(&chat.pane_id))
        })
    }

    pub(super) fn interrupt_local_chat(&mut self) {
        self.local_chat_generation = self.local_chat_generation.saturating_add(1);
        if let Some(chat) = self.local_chat.as_mut() {
            chat.generation = self.local_chat_generation;
            chat.in_flight = false;
            chat.last_revision = None;
            if let Some(sent) = chat.sending.take() {
                chat.draft = format!(
                    "{sent}{}{}",
                    if chat.draft.is_empty() { "" } else { "\n" },
                    chat.draft
                );
                chat.cursor = chat.draft.len();
                chat.draft_dirty = true;
                chat.uncertain = true;
                chat.status = "Connection interrupted; submission unconfirmed. Inspect terminal before Ctrl+R retry.".into();
            }
        }
    }

    pub(super) fn toggle_local_chat(&mut self, outcome: &mut ClientShellInput) {
        if self.local_chat_matches() {
            if let Some(chat) = &mut self.local_chat {
                chat.active = !chat.active;
            }
            outcome.repaint = true;
            return;
        }
        // A draft may belong to a different remote pane. Never silently replace it.
        if self
            .local_chat
            .as_ref()
            .is_some_and(|c| !c.draft.is_empty() || c.sending.is_some())
        {
            self.endpoint_error = Some(
                "Local chat has a draft in another pane. Return there to send or clear it first."
                    .into(),
            );
            outcome.repaint = true;
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let Some(pane_id) = snapshot.focused_pane_id.clone() else {
            return;
        };
        if !snapshot
            .agents
            .iter()
            .any(|a| a.pane_id == pane_id && a.agent.as_deref() == Some("claude"))
        {
            self.endpoint_error = Some("Local chat requires a recognized Claude Code pane. Start Claude in this terminal first.".into());
            outcome.repaint = true;
            return;
        }
        let methods = self
            .endpoints
            .iter()
            .find(|e| e.endpoint_id == self.active_endpoint_id)
            .and_then(|e| e.methods.as_ref());
        if methods.is_some_and(|m| {
            !m.iter().any(|s| s == "pane.read") || !m.iter().any(|s| s == "agent.prompt")
        }) {
            self.endpoint_error = Some("This server does not support local chat yet. Use the preview server session; existing terminals remain available.".into());
            outcome.repaint = true;
            return;
        }
        self.local_chat_generation = self.local_chat_generation.saturating_add(1);
        self.local_chat = Some(LocalChat {
            active: true,
            endpoint: self.active_endpoint_id.clone(),
            boot_id: snapshot.boot_id.clone(),
            pane_id,
            generation: self.local_chat_generation,
            draft: String::new(),
            cursor: 0,
            sending: None,
            uncertain: false,
            text: String::new(),
            wrapped: Vec::new(),
            pending_history: None,
            draft_layout: Vec::new(),
            draft_prefix: Vec::new(),
            draft_dirty: true,
            width: 80,
            offset: 0,
            viewport_rows: 1,
            status: "Loading remote terminal history…".into(),
            truncated: false,
            in_flight: false,
            last_revision: None,
            next_refresh: Instant::now(),
            area: Rect::default(),
        });
        self.tick_local_chat(Instant::now(), outcome);
        outcome.repaint = true;
    }

    pub(crate) fn tick_local_chat(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        if !self.local_chat_matches() {
            return;
        }
        let Some(chat) = self
            .local_chat
            .as_ref()
            .filter(|c| c.active && !c.in_flight && now >= c.next_refresh)
        else {
            return;
        };
        if !self.endpoint_is_online(&self.active_endpoint_id) {
            return;
        }
        if self
            .snapshot
            .as_deref()
            .is_none_or(|s| s.boot_id != chat.boot_id)
        {
            return;
        }
        let revision = self
            .pane_surface
            .as_ref()
            .and_then(|s| s.panes.iter().find(|p| p.pane_id == chat.pane_id))
            .map(|p| p.content_revision);
        if chat.last_revision.is_some() && revision == chat.last_revision {
            return;
        }
        let generation = chat.generation;
        let method = Method::PaneRead(PaneReadParams {
            pane_id: chat.pane_id.clone(),
            source: ReadSource::RecentUnwrapped,
            lines: Some(READ_LINES),
            format: ReadFormat::Text,
            strip_ansi: true,
            intent: crate::api::schema::ReadIntent::Passive,
        });
        let sent = self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::LocalChatRead { generation },
            outcome,
        );
        if let Some(chat) = self.local_chat.as_mut() {
            chat.in_flight = sent;
            chat.last_revision = if sent { revision } else { None };
            chat.next_refresh = now + REFRESH;
        }
    }

    pub(super) fn complete_local_chat_read(
        &mut self,
        generation: u64,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(chat) = self
            .local_chat
            .as_mut()
            .filter(|c| c.generation == generation)
        else {
            return false;
        };
        chat.in_flight = false;
        match result {
            Ok(ResponseResult::PaneRead { read }) if read.pane_id == chat.pane_id => {
                chat.update_text(read.text, read.truncated);
                if chat.sending.is_none() && !chat.uncertain {
                    chat.status = "Live terminal history cached locally".into();
                }
            }
            Ok(_) => {
                chat.last_revision = None;
                chat.status = "Unexpected history reply; cached content retained".into();
            }
            Err(error) => {
                chat.last_revision = None;
                chat.status = format!("History unavailable: {}", error.message);
            }
        }
        chat.active
    }

    fn submit_local_chat(&mut self, outcome: &mut ClientShellInput) {
        let Some(chat) = self.local_chat.as_ref() else {
            return;
        };
        if chat.draft.trim().is_empty() || chat.sending.is_some() || chat.uncertain {
            return;
        }
        if self
            .snapshot
            .as_deref()
            .is_none_or(|s| s.boot_id != chat.boot_id)
        {
            if let Some(chat) = self.local_chat.as_mut() {
                chat.status =
                    "Remote server changed. Inspect terminal, then Ctrl+R to reconnect this draft."
                        .into();
            }
            return;
        }
        let generation = chat.generation;
        let text = chat.draft.clone();
        let method = Method::AgentPrompt(AgentPromptParams {
            target: chat.pane_id.clone(),
            text: text.clone(),
            wait: None,
        });
        if self.push_endpoint_method_with_kind(
            method,
            PendingEndpointKind::LocalChatSend { generation },
            outcome,
        ) {
            if let Some(chat) = self.local_chat.as_mut() {
                chat.sending = Some(text);
                chat.draft.clear();
                chat.draft_dirty = true;
                chat.cursor = 0;
                chat.status = "Submitting… awaiting remote acknowledgement".into();
            }
        }
    }

    pub(super) fn complete_local_chat_send(
        &mut self,
        generation: u64,
        result: Result<ResponseResult, ClientShellEndpointError>,
    ) -> bool {
        let Some(chat) = self
            .local_chat
            .as_mut()
            .filter(|c| c.generation == generation)
        else {
            return false;
        };
        let sent = chat.sending.take().unwrap_or_default();
        match result {
            Ok(ResponseResult::AgentPrompted { .. }) => {
                chat.status = "Submitted to Claude; waiting for output".into();
                chat.last_revision = None;
            }
            other => {
                // An unconfirmed submission is never retried automatically, including reconnects.
                let message = match other {
                    Err(e) => e.message,
                    _ => "Unexpected submit acknowledgement".into(),
                };
                chat.draft = format!(
                    "{sent}{}{}",
                    if chat.draft.is_empty() { "" } else { "\n" },
                    chat.draft
                );
                chat.cursor = chat.draft.len();
                chat.draft_dirty = true;
                chat.uncertain = true;
                chat.status = format!("Send not confirmed: {message}. Esc: inspect terminal. Ctrl+R: allow manual retry.");
            }
        }
        chat.active
    }

    pub(super) fn handle_local_chat_event(
        &mut self,
        event: &RawInputEvent,
        outcome: &mut ClientShellInput,
    ) -> bool {
        if let RawInputEvent::Key(key) = event {
            if key.code == KeyCode::F(6)
                && key.kind == KeyEventKind::Press
                && self.overlay.is_none()
                && self.mode == ClientShellMode::Terminal
            {
                let recognized = self.snapshot.as_deref().is_some_and(|s| {
                    s.agents.iter().any(|a| {
                        Some(&a.pane_id) == s.focused_pane_id.as_ref()
                            && a.agent.as_deref() == Some("claude")
                    })
                });
                if self.local_chat_matches() || recognized {
                    self.toggle_local_chat(outcome);
                    return true;
                }
                return false;
            }
        }
        if !self.local_chat_matches()
            || self.overlay.is_some()
            || !self.local_chat.as_ref().is_some_and(|c| c.active)
        {
            return false;
        }
        let current_boot = self.snapshot.as_deref().map(|s| s.boot_id.clone());
        let chat = self.local_chat.as_mut().expect("checked local chat");
        match event {
            RawInputEvent::Text(text) => chat.insert(text.as_str()),
            RawInputEvent::Paste(text) => chat.insert(text),
            RawInputEvent::Key(key) => {
                if key.kind == KeyEventKind::Release {
                    return true;
                }
                chat.draft_dirty = true;
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                match key.code {
                    KeyCode::Esc => chat.active = false,
                    KeyCode::Enter
                        if key
                            .modifiers
                            .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
                    {
                        chat.insert("\n")
                    }
                    KeyCode::Char('j') if ctrl => chat.insert("\n"),
                    KeyCode::Enter => self.submit_local_chat(outcome),
                    KeyCode::Up => chat.move_vertical(true),
                    KeyCode::Down => chat.move_vertical(false),
                    KeyCode::Left => chat.cursor = chat.previous(),
                    KeyCode::Right => chat.cursor = chat.next(),
                    KeyCode::Home if ctrl => {
                        chat.offset = chat.wrapped.len().saturating_sub(chat.viewport_rows)
                    }
                    KeyCode::End if ctrl => chat.resume_live(),
                    KeyCode::Home => {
                        chat.cursor = chat.draft[..chat.cursor].rfind('\n').map_or(0, |n| n + 1)
                    }
                    KeyCode::End => {
                        chat.cursor = chat.draft[chat.cursor..]
                            .find('\n')
                            .map_or(chat.draft.len(), |n| chat.cursor + n)
                    }
                    KeyCode::Backspace => {
                        let previous = chat.previous();
                        chat.draft.drain(previous..chat.cursor);
                        chat.cursor = previous;
                    }
                    KeyCode::Delete => {
                        chat.draft.drain(chat.cursor..chat.next());
                    }
                    KeyCode::PageUp => chat.scroll(chat.viewport_rows.saturating_sub(1) as isize),
                    KeyCode::PageDown => {
                        chat.scroll(-(chat.viewport_rows.saturating_sub(1) as isize))
                    }
                    KeyCode::Char('y') if ctrl => outcome.actions.push(
                        ClientShellAction::ClipboardWrite(chat.text.as_bytes().to_vec()),
                    ),
                    KeyCode::Char('r') if ctrl => {
                        if let Some(boot) = current_boot {
                            chat.boot_id = boot;
                        }
                        chat.last_revision = None;
                        chat.uncertain = false;
                        chat.status = "Manual retry enabled; Enter submits the draft".into();
                    }
                    KeyCode::Char('u') if ctrl => {
                        chat.draft.clear();
                        chat.draft_dirty = true;
                        chat.cursor = 0;
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        chat.insert(&c.to_string())
                    }
                    _ => {}
                }
            }
            RawInputEvent::Mouse(mouse)
                if super::contains(chat.area, (mouse.column, mouse.row)) =>
            {
                match mouse.kind {
                    MouseEventKind::ScrollUp => chat.scroll(3),
                    MouseEventKind::ScrollDown => chat.scroll(-3),
                    _ => {}
                }
            }
            _ => return false,
        }
        outcome.repaint = true;
        true
    }

    pub(super) fn compose_local_chat(&mut self, frame: &mut FrameData, area: Rect) {
        if !self.local_chat_matches() || self.overlay.is_some() {
            return;
        }
        let Some(chat) = self.local_chat.as_mut().filter(|c| c.active) else {
            return;
        };
        let Some(mut buffer) = frame.to_ratatui_buffer() else {
            return;
        };
        chat.area = area;
        if area.width < 12 || area.height < 8 {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    buffer[(x, y)].reset();
                }
            }
            render::put_text(
                &mut buffer,
                area.x,
                area.y,
                area.width,
                "Local chat: enlarge window or Esc",
                Style::default(),
            );
            frame.replace_from_ratatui_buffer_preserving_effects(&buffer, None);
            return;
        }
        let palette = &self.config.palette;
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].reset();
            }
        }
        buffer.set_style(area, Style::default().fg(palette.text).bg(palette.panel_bg));
        let width = area.width.saturating_sub(2);
        if width != chat.width {
            chat.draft_dirty = true;
            chat.width = width;
            chat.wrapped = wrap(&chat.text, width);
        }
        if chat.draft_dirty || chat.width != width || chat.draft_layout.is_empty() {
            chat.draft_layout = wrap(&chat.draft, width);
            chat.draft_prefix = wrap(&chat.draft[..chat.cursor], width);
            chat.draft_dirty = false;
        }
        let draft = &chat.draft_layout;
        let prefix = &chat.draft_prefix;
        let cursor_line = prefix.len().saturating_sub(1);
        let draft_rows = draft
            .len()
            .clamp(2, 5)
            .min(usize::from(area.height.saturating_sub(6)));
        chat.viewport_rows = usize::from(area.height).saturating_sub(draft_rows + 4);
        let max_offset = chat.wrapped.len().saturating_sub(chat.viewport_rows);
        chat.offset = chat.offset.min(max_offset);
        let top = max_offset.saturating_sub(chat.offset);
        let title = format!(
            "Local chat · {} · {}{}{}",
            chat.pane_id,
            if chat.offset == 0 {
                "live"
            } else {
                "reading cache"
            },
            if chat.pending_history.is_some() {
                " · new output (Ctrl+End)"
            } else {
                ""
            },
            if chat.truncated {
                " · older history unavailable"
            } else {
                ""
            }
        );
        render::put_text(
            &mut buffer,
            area.x + 1,
            area.y,
            width,
            &title,
            Style::default().fg(palette.accent),
        );
        for (row, line) in chat
            .wrapped
            .iter()
            .skip(top)
            .take(chat.viewport_rows)
            .enumerate()
        {
            render::put_text(
                &mut buffer,
                area.x + 1,
                area.y + 1 + row as u16,
                width,
                line,
                Style::default().fg(palette.text),
            );
        }
        let input_y = area.y + 1 + chat.viewport_rows as u16;
        render::put_text(
            &mut buffer,
            area.x + 1,
            input_y,
            width,
            &chat.status,
            Style::default().fg(palette.overlay0),
        );
        let draft_top = cursor_line.saturating_sub(draft_rows.saturating_sub(1));
        for (row, line) in draft.iter().skip(draft_top).take(draft_rows).enumerate() {
            render::put_text(
                &mut buffer,
                area.x + 1,
                input_y + 1 + row as u16,
                width,
                line,
                Style::default().fg(palette.text),
            );
        }
        render::put_text(
            &mut buffer,
            area.x + 1,
            area.bottom() - 1,
            width,
            "Enter send · Ctrl+J newline · PgUp/PgDn read · Ctrl+Y copy · Esc terminal",
            Style::default().fg(palette.overlay0),
        );
        let x = prefix
            .last()
            .map_or(0, |l| UnicodeWidthStr::width(l.as_str()))
            .min(usize::from(width.saturating_sub(1)));
        let cursor = crate::protocol::CursorState {
            x: area.x + 1 + x as u16,
            y: input_y + 1 + cursor_line.saturating_sub(draft_top) as u16,
            visible: true,
            shape: 6,
        };
        frame.replace_from_ratatui_buffer_preserving_effects(&buffer, Some(cursor));
    }
}
