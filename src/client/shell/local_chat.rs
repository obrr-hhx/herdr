//! Client-owned draft and bounded reader. Nothing here predicts remote terminal state.
use super::*;
use crate::api::schema::{
    AgentPromptParams, Method, PaneReadParams, ReadFormat, ReadSource, ResponseResult,
};
use crate::raw_input::RawInputEvent;
use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthChar;

mod echo;
mod transcript;
mod view;
use transcript::{LineKind, Transcript};

const MAX_DRAFT_BYTES: usize = 64 * 1024;
const MAX_CACHE_BYTES: usize = 1024 * 1024;
const READ_LINES: u32 = 2000;
const REFRESH: Duration = Duration::from_millis(500);

pub(super) struct LocalChat {
    echoes: VecDeque<echo::LocalEcho>,
    echo_dirty: bool,
    echo_lines: Vec<String>,
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
    pub(super) width: u16,
    pub(super) offset: usize,
    viewport_rows: usize,
    pub(super) status: String,
    truncated: bool,
    in_flight: bool,
    last_revision: Option<u64>,
    next_refresh: Instant,
    pub(super) area: Rect,
    pub(super) reader_area: Rect,
    pub(super) editor_area: Rect,
    draft_top: usize,
    pub(super) transcript: Transcript,
    document_lines: Vec<(String, LineKind)>,
    wrapped_styles: Vec<LineKind>,
    pending_document: Option<transcript::Document>,
    document_anchors: Vec<(String, usize)>,
    live_preview: Vec<String>,
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

// Only remove Claude's framed composer control at the very end. Preserve
// prompt markers and rules inside actual answers and user messages.
fn history_text(text: String) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let rule = |line: &str| {
        let line = line.trim();
        !line.is_empty() && line.chars().all(|c| c == '─')
    };
    if let Some(index) = lines
        .windows(3)
        .rposition(|rows| rule(rows[0]) && rows[1].trim_start().starts_with('❯') && rule(rows[2]))
    {
        // Footer controls must be near the tail, not an earlier quoted block.
        if lines.len().saturating_sub(index) <= 12 {
            return lines[..index].join("\n").trim_end().to_string();
        }
    }
    text
}

fn merge_history(previous: &str, incoming: String) -> (String, bool) {
    if previous.is_empty() || incoming == previous {
        return (incoming, false);
    }
    if incoming.trim().is_empty() {
        return (previous.to_owned(), false);
    }
    let mut nonempty = 0;
    let mut end = 0;
    let mut start = None;
    for line in incoming.split_inclusive('\n') {
        if !line.trim().is_empty() {
            start.get_or_insert(end);
            nonempty += 1;
        }
        end += line.len();
        if nonempty == 3 {
            break;
        }
    }
    if let Some(start) = start {
        let anchor = &incoming[start..end];
        // Three complete, nonempty lines are evidence for the same terminal window.
        // Short/repeated spinner fragments are not safe overlap anchors.
        if nonempty >= 3 && anchor.len() >= 24 {
            if let Some(at) = previous.rfind(anchor) {
                return (format!("{}{}", &previous[..at], &incoming[start..]), false);
            }
        }
    }
    (incoming, true)
}

fn wrap_starts(text: &str, width: u16) -> Vec<usize> {
    let mut starts = vec![0];
    let mut column = 0;
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
            column = 0;
        } else if !ch.is_control() || ch == '\t' {
            let cells = if ch == '\t' {
                1
            } else {
                ch.width().unwrap_or(0)
            };
            if column + cells > usize::from(width.max(1)) && column > 0 {
                starts.push(index);
                column = 0;
            }
            column += cells;
        }
    }
    starts
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

    fn update_text(&mut self, text: String, truncated: bool) {
        let incoming = history_text(text);
        let previous = self
            .pending_history
            .as_ref()
            .map_or(self.text.as_str(), |(text, _)| text.as_str());
        let (mut text, gap) = merge_history(previous, incoming);
        let mut truncated = truncated || gap;
        // Retain at most 2,000 real terminal lines across overlapping alternate-screen
        // windows. No guessed user/assistant messages or screenshot concatenation.
        if let Some((cut, _)) = text.match_indices('\n').rev().nth(2000) {
            text.drain(..cut + 1);
            truncated = true;
        }
        let mut cut = text.len().saturating_sub(MAX_CACHE_BYTES);
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        if cut > 0 {
            text.drain(..cut);
        }
        let truncated = truncated || cut > 0 || self.truncated;
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
        self.resume_transcript();
        if self.transcript.loaded {
            self.transcript.revision.clear();
            self.transcript.next = None;
        }
        if let Some((text, truncated)) = self.pending_history.take() {
            self.update_text(text, truncated);
        }
    }

    fn move_vertical(&mut self, up: bool) {
        let mut prefix = wrap(&self.draft[..self.cursor], self.width);
        if prefix
            .last()
            .is_some_and(|line| UnicodeWidthStr::width(line.as_str()) == usize::from(self.width))
        {
            prefix.push(String::new());
        }
        let row = prefix.len().saturating_sub(1);
        let column = prefix
            .last()
            .map_or(0, |line| UnicodeWidthStr::width(line.as_str()));
        let target = if up {
            row.checked_sub(1)
        } else {
            Some(row + 1)
        };
        if let Some(target) = target.filter(|target| *target < wrap(&self.draft, self.width).len())
        {
            self.cursor = draft_position(&self.draft, self.width, target, column);
        }
    }
}

impl ClientShellState {
    pub(crate) fn local_chat_visible(&self) -> bool {
        self.overlay.is_none()
            && self.mode == ClientShellMode::Terminal
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
            chat.transcript.busy = false;
            chat.transcript.next = None;
            if let Some(sent) = chat.sending.take() {
                chat.acknowledge_echo(false);
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
            echoes: VecDeque::new(),
            echo_dirty: true,
            echo_lines: Vec::new(),
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
            reader_area: Rect::default(),
            editor_area: Rect::default(),
            draft_top: 0,
            transcript: Transcript::default(),
            document_lines: Vec::new(),
            wrapped_styles: Vec::new(),
            pending_document: None,
            document_anchors: Vec::new(),
            live_preview: Vec::new(),
        });
        self.tick_local_chat(Instant::now(), outcome);
        outcome.repaint = true;
    }

    pub(crate) fn tick_local_chat(&mut self, now: Instant, outcome: &mut ClientShellInput) {
        if self.activation_preview.is_some() || !self.local_chat_matches() {
            return;
        }
        let bound_boot = self
            .local_chat
            .as_ref()
            .zip(self.snapshot.as_deref())
            .is_some_and(|(c, s)| c.boot_id == s.boot_id);
        if self.history_source_enabled
            && bound_boot
            && self.endpoint_is_online(&self.active_endpoint_id)
        {
            if let Some(chat) = self.local_chat.as_mut().filter(|c| {
                c.active && !c.transcript.busy && c.transcript.next.is_none_or(|next| now >= next)
            }) {
                chat.transcript.busy = true;
                chat.transcript.next = Some(
                    now + if chat.sending.is_some() || !chat.echoes.is_empty() {
                        Duration::from_millis(250)
                    } else {
                        Duration::from_secs(1)
                    },
                );
                outcome.actions.push(ClientShellAction::ReadAgentHistory {
                    endpoint: chat.endpoint.clone(),
                    boot: chat.boot_id.clone(),
                    generation: chat.generation,
                    pane: chat.pane_id.clone(),
                    before: chat.transcript.want_older.then_some(chat.transcript.before),
                    known: chat.transcript.revision.clone(),
                });
            }
        }
        if self.history_source_enabled {
            self.refresh_local_chat_surface(now, outcome);
            return;
        }
        let preview_active = self.snapshot.as_deref().is_some_and(|s| {
            s.agents.iter().any(|agent| {
                Some(&agent.pane_id) == s.focused_pane_id.as_ref()
                    && matches!(
                        agent.agent_status,
                        crate::api::schema::AgentStatus::Working
                            | crate::api::schema::AgentStatus::Blocked
                    )
            })
        });
        if self
            .local_chat
            .as_ref()
            .is_some_and(|c| c.transcript.loaded)
            && !preview_active
        {
            if let Some(chat) = self.local_chat.as_mut() {
                if !chat.live_preview.is_empty() {
                    chat.live_preview.clear();
                    outcome.repaint = true;
                }
            }
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
        if chat.transcript.loaded {
            if let Ok(ResponseResult::PaneRead { read }) = result {
                if read.pane_id == chat.pane_id {
                    let text = history_text(read.text);
                    chat.live_preview = text
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .rev()
                        .take(5)
                        .map(|line| line.chars().take(4096).collect::<String>())
                        .collect();
                    chat.live_preview.reverse();
                }
            }
            return chat.active && chat.offset == 0;
        }
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
        if chat.echoes.len() >= 16 {
            if let Some(chat) = self.local_chat.as_mut() {
                chat.status = "Waiting for saved history to catch up; draft retained".into();
            }
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
                chat.stage_echo(text.clone());
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
        let Some(sent) = chat.sending.take() else {
            return false;
        };
        chat.acknowledge_echo(matches!(&result, Ok(ResponseResult::AgentPrompted { .. })));
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
        if self.activation_preview.is_some() {
            return false;
        }
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
            || self.mode != ClientShellMode::Terminal
            || self.overlay.is_some()
            || !self.local_chat.as_ref().is_some_and(|c| c.active)
        {
            return false;
        }
        if let RawInputEvent::Key(key) = event {
            if crate::config::terminal_key_matches_combo(key, self.config.keybinds.prefix) {
                return false;
            }
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
                    MouseEventKind::ScrollUp
                        if super::contains(chat.reader_area, (mouse.column, mouse.row)) =>
                    {
                        chat.scroll(3)
                    }
                    MouseEventKind::ScrollDown
                        if super::contains(chat.reader_area, (mouse.column, mouse.row)) =>
                    {
                        chat.scroll(-3)
                    }
                    MouseEventKind::Down(MouseButton::Left)
                        if super::contains(chat.editor_area, (mouse.column, mouse.row)) =>
                    {
                        let row = chat.draft_top + usize::from(mouse.row - chat.editor_area.y);
                        let column = usize::from(mouse.column - chat.editor_area.x);
                        chat.cursor = draft_position(&chat.draft, chat.width, row, column);
                        chat.draft_dirty = true;
                    }
                    MouseEventKind::Down(MouseButton::Left) if mouse.row == chat.area.y => {
                        chat.resume_live()
                    }
                    _ => {}
                }
            }
            _ => return false,
        }
        let upward = matches!(event, RawInputEvent::Key(key) if key.code==KeyCode::PageUp || (key.code==KeyCode::Home && key.modifiers.contains(KeyModifiers::CONTROL)))
            || matches!(event, RawInputEvent::Mouse(mouse) if mouse.kind==MouseEventKind::ScrollUp);
        if upward {
            if let Some(chat) = self.local_chat.as_mut() {
                chat.request_older();
            }
        }
        outcome.repaint = true;
        true
    }
}

// Resolve a click using the same cell widths and wrap rules as the editor.
fn draft_position(text: &str, width: u16, target_row: usize, target_column: usize) -> usize {
    let mut row = 0;
    let mut column = 0;
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            if row == target_row {
                return index;
            }
            row += 1;
            column = 0;
            continue;
        }
        let cells = if ch == '\t' {
            1
        } else {
            ch.width().unwrap_or(0)
        };
        if column + cells > usize::from(width) && column > 0 {
            row += 1;
            column = 0;
        }
        if row == target_row && column + cells > target_column {
            return index;
        }
        if row > target_row {
            return index;
        }
        column += cells;
    }
    text.len()
}
