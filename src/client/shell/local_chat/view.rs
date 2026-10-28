//! Local reader/editor layout, using the existing shell palette and pane geometry.
use super::*;

impl ClientShellState {
    pub(in crate::client::shell) fn local_chat_area(&self, area: Rect) -> Rect {
        // Preserve neighbouring panes and their borders. Retained geometry remains useful
        // while a new projection is in flight; clip it to the current host size.
        self.pane_surface
            .as_ref()
            .filter(|s| s.panes.len() > 1)
            .and_then(|s| {
                s.panes.iter().find(|p| {
                    self.local_chat
                        .as_ref()
                        .is_some_and(|c| c.pane_id == p.pane_id)
                })
            })
            .map_or(area, |p| {
                Rect::new(
                    area.x.saturating_add(p.inner_rect.x),
                    area.y.saturating_add(p.inner_rect.y),
                    p.inner_rect.width,
                    p.inner_rect.height,
                )
                .intersection(area)
            })
    }

    pub(in crate::client::shell) fn compose_local_chat(
        &mut self,
        frame: &mut FrameData,
        area: Rect,
    ) {
        if !self.local_chat_visible() {
            return;
        }
        let area = self.local_chat_area(area);
        let status = self
            .snapshot
            .as_deref()
            .and_then(|snapshot| {
                snapshot
                    .agents
                    .iter()
                    .find(|agent| Some(&agent.pane_id) == snapshot.focused_pane_id.as_ref())
            })
            .map(|agent| agent.agent_status);
        let Some(chat) = self.local_chat.as_mut() else {
            return;
        };
        Self::compose_local_chat_frame(chat, &self.config.palette, status, frame, area);
    }

    pub(in crate::client::shell) fn compose_local_chat_frame(
        chat: &mut LocalChat,
        palette: &Palette,
        status: Option<crate::api::schema::AgentStatus>,
        frame: &mut FrameData,
        area: Rect,
    ) {
        let working = status == Some(crate::api::schema::AgentStatus::Working);
        let blocked = status == Some(crate::api::schema::AgentStatus::Blocked);
        let Some(mut buffer) = frame.to_ratatui_buffer() else {
            return;
        };
        let geometry_changed = chat.area != area;
        chat.area = area;
        chat.reader_area = Rect::default();
        chat.editor_area = Rect::default();
        let base = Style::default().fg(palette.text).bg(palette.panel_bg);
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                buffer[(x, y)].reset();
                buffer[(x, y)].set_style(base);
            }
        }
        if area.width < 16 || area.height < 7 {
            render::put_text(
                &mut buffer,
                area.x,
                area.y,
                area.width,
                "Enlarge · Esc",
                base.fg(palette.overlay0),
            );
            frame.replace_from_ratatui_buffer_preserving_effects(&buffer, None);
            return;
        }
        let anchor = (geometry_changed && chat.offset > 0).then(|| {
            let top = chat
                .wrapped
                .len()
                .saturating_sub(chat.viewport_rows)
                .saturating_sub(chat.offset);
            chat.history_row_starts.get(top).copied().unwrap_or(0)
        });
        let width = area.width - 4;
        if width != chat.width {
            chat.echo_dirty = true;
            chat.draft_dirty = true;
            chat.width = width;
            chat.rewrap_history();
        }
        if chat.draft_dirty || chat.draft_layout.is_empty() {
            chat.draft_layout = wrap(&chat.draft, width);
            chat.draft_prefix = wrap(&chat.draft[..chat.cursor], width);
            // At the exact right edge the insertion point is on the next visual row.
            if chat
                .draft_prefix
                .last()
                .is_some_and(|s| UnicodeWidthStr::width(s.as_str()) == usize::from(width))
            {
                chat.draft_prefix.push(String::new());
                if chat.cursor == chat.draft.len() {
                    chat.draft_layout.push(String::new());
                }
            }
            chat.draft_dirty = false;
        }
        if chat.echo_dirty {
            chat.echo_lines = chat
                .echoes
                .back()
                .map(|echo| {
                    let mut lines = wrap(&echo.text, width.saturating_sub(2));
                    if lines.len() > 6 {
                        lines.truncate(5);
                        lines.push("… [message continues]".into());
                    }
                    lines
                        .into_iter()
                        .enumerate()
                        .map(|(row, line)| format!("{}{line}", if row == 0 { "❯ " } else { "  " }))
                        .collect()
                })
                .unwrap_or_default();
            chat.echo_dirty = false;
        }
        let cursor_line = chat.draft_prefix.len().saturating_sub(1);
        let draft_rows = chat
            .draft_layout
            .len()
            .clamp(1, 6)
            .min(usize::from(area.height.saturating_sub(6).max(1)));
        let box_y = area.bottom() - draft_rows as u16 - 3;
        chat.reader_area = Rect::new(
            area.x + 2,
            area.y + 2,
            width,
            box_y.saturating_sub(area.y + 2),
        );
        let echo_rows = if chat.offset == 0 && !chat.echo_lines.is_empty() {
            (chat.echo_lines.len() as u16 + 1).min(chat.reader_area.height.saturating_sub(3))
        } else {
            0
        };
        let preview_rows = if (working || blocked)
            && chat.transcript.loaded
            && chat.offset == 0
            && !chat.live_preview.is_empty()
            && chat.reader_area.height > 8
        {
            (chat.live_preview.len() as u16 + 1)
                .min(6)
                .min(chat.reader_area.height.saturating_sub(echo_rows + 3))
        } else {
            0
        };
        chat.reader_area.height = chat
            .reader_area
            .height
            .saturating_sub(preview_rows + echo_rows);
        chat.viewport_rows = usize::from(chat.reader_area.height);
        let max_offset = chat.wrapped.len().saturating_sub(chat.viewport_rows);
        if let Some(anchor) = anchor {
            let top = chat
                .history_row_starts
                .partition_point(|position| *position <= anchor)
                .saturating_sub(1);
            chat.offset = max_offset.saturating_sub(top.min(max_offset));
        }
        chat.offset = chat.offset.min(max_offset);
        let top = max_offset.saturating_sub(chat.offset);
        let indicator = if chat.pending_history.is_some() || chat.pending_document.is_some() {
            "Reading · New output ↓".to_string()
        } else if chat.offset > 0 {
            format!(
                "Reading {}–{} / {}",
                top + 1,
                (top + chat.viewport_rows).min(chat.wrapped.len()),
                chat.wrapped.len()
            )
        } else if blocked {
            "Needs attention · Esc terminal".to_string()
        } else if chat.transcript.loaded {
            String::new()
        } else {
            "Loading conversation…".to_string()
        };
        let indicator_width = UnicodeWidthStr::width(indicator.as_str());
        if !indicator.is_empty() && usize::from(area.width) > indicator_width + 4 {
            render::put_text(
                &mut buffer,
                area.right() - 2 - indicator_width as u16,
                area.y,
                indicator_width as u16,
                &indicator,
                base.fg(palette.overlay0),
            );
        }
        for (row, line) in chat
            .wrapped
            .iter()
            .skip(top)
            .take(chat.viewport_rows)
            .enumerate()
        {
            let kind = chat
                .wrapped_styles
                .get(top + row)
                .copied()
                .unwrap_or_default();
            let style = transcript::line_style(kind, palette);
            if kind == LineKind::User {
                buffer.set_style(
                    Rect::new(
                        chat.reader_area.x,
                        chat.reader_area.y + row as u16,
                        width,
                        1,
                    ),
                    style,
                );
            }
            render::put_text(
                &mut buffer,
                chat.reader_area.x,
                chat.reader_area.y + row as u16,
                width,
                line,
                style,
            );
        }
        if preview_rows > 0 {
            let y = chat.reader_area.bottom();
            render::put_text(
                &mut buffer,
                chat.reader_area.x,
                y,
                width,
                if blocked {
                    "Needs attention · Esc terminal"
                } else {
                    "✻ Working…"
                },
                base.fg(palette.overlay0),
            );
            for (row, line) in chat
                .live_preview
                .iter()
                .take(usize::from(preview_rows - 1))
                .enumerate()
            {
                render::put_text(
                    &mut buffer,
                    chat.reader_area.x,
                    y + 1 + row as u16,
                    width,
                    line,
                    base.fg(palette.overlay1),
                );
            }
        }
        if echo_rows > 0 {
            let content_end = chat.reader_area.y
                + chat
                    .wrapped
                    .len()
                    .saturating_sub(top)
                    .min(chat.viewport_rows) as u16;
            let y = if content_end.saturating_add(echo_rows) <= chat.reader_area.bottom() {
                content_end
            } else {
                chat.reader_area.bottom() + preview_rows
            };
            for (row, line) in chat
                .echo_lines
                .iter()
                .take(usize::from(echo_rows - 1))
                .enumerate()
            {
                let style = transcript::line_style(LineKind::User, palette);
                buffer.set_style(
                    Rect::new(chat.reader_area.x, y + row as u16, width, 1),
                    style,
                );
                render::put_text(
                    &mut buffer,
                    chat.reader_area.x,
                    y + row as u16,
                    width,
                    line,
                    style,
                );
            }
            render::put_text(
                &mut buffer,
                chat.reader_area.x,
                y + echo_rows - 1,
                width,
                if chat.echoes.back().is_some_and(|echo| echo.acknowledged) {
                    "  Syncing history…"
                } else {
                    "✻ Sending…"
                },
                base.fg(palette.overlay0),
            );
        }
        if max_offset > 0 && chat.viewport_rows > 0 {
            let thumb = (chat.viewport_rows * chat.viewport_rows / chat.wrapped.len()).max(1);
            let position = top * chat.viewport_rows.saturating_sub(thumb) / max_offset;
            for row in 0..chat.viewport_rows {
                render::put_text(
                    &mut buffer,
                    area.right() - 2,
                    chat.reader_area.y + row as u16,
                    1,
                    if (position..position + thumb).contains(&row) {
                        "┃"
                    } else {
                        "│"
                    },
                    base.fg(palette.surface_dim),
                );
            }
        }
        let edge = base.fg(if chat.uncertain {
            palette.accent
        } else {
            palette.overlay0
        });
        let box_width = area.width - 2;
        render::put_text(
            &mut buffer,
            area.x + 1,
            box_y,
            box_width,
            &"─".repeat(usize::from(box_width)),
            edge,
        );
        render::put_text(
            &mut buffer,
            area.x + 1,
            area.bottom() - 2,
            box_width,
            &"─".repeat(usize::from(box_width)),
            edge,
        );
        let title = if chat.uncertain { " Ctrl+R " } else { "" };
        if !title.is_empty() && box_width > title.len() as u16 + 4 {
            render::put_text(
                &mut buffer,
                area.x + 3,
                box_y,
                title.len() as u16,
                title,
                edge,
            );
        }
        chat.editor_area = Rect::new(area.x + 3, box_y + 1, width, draft_rows as u16);
        chat.draft_top = cursor_line.saturating_sub(draft_rows.saturating_sub(1));
        for row in 0..draft_rows {
            let y = box_y + 1 + row as u16;
            if row == 0 {
                render::put_text(&mut buffer, area.x + 1, y, 1, "❯", edge);
            }
            if let Some(line) = chat.draft_layout.get(chat.draft_top + row) {
                render::put_text(&mut buffer, chat.editor_area.x, y, width, line, base);
            }
        }
        let help = if chat.uncertain
            || chat.status.starts_with("History unavailable")
            || chat.status.starts_with("Remote server")
            || chat.status.starts_with("Draft limit")
            || chat.status.starts_with("Saved history unavailable")
            || chat.status.starts_with("Waiting for saved history")
        {
            chat.status.as_str()
        } else if width >= 76 {
            "Ctrl+J newline · PgUp/PgDn history · Ctrl+End live · Esc terminal"
        } else if width >= 48 {
            "Ctrl+J newline · PgUp history · Esc terminal"
        } else if width >= 26 {
            "Enter send · Esc terminal"
        } else {
            "Enter · Esc"
        };
        render::put_text(
            &mut buffer,
            area.x + 2,
            area.bottom() - 1,
            width,
            help,
            base.fg(if chat.uncertain {
                palette.accent
            } else {
                palette.overlay0
            }),
        );
        let x = chat
            .draft_prefix
            .last()
            .map_or(0, |s| UnicodeWidthStr::width(s.as_str()))
            .min(usize::from(width - 1));
        let cursor = crate::protocol::CursorState {
            x: chat.editor_area.x + x as u16,
            y: chat.editor_area.y + cursor_line.saturating_sub(chat.draft_top) as u16,
            visible: true,
            shape: 6,
        };
        frame.replace_from_ratatui_buffer_preserving_effects(&buffer, Some(cursor));
    }
}
