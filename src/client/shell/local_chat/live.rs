//! Latest native terminal body. Saved transcript paging remains a separate reader.
use super::*;
use ratatui::buffer::Buffer;

#[derive(Default)]
pub(super) struct LiveSurface {
    pub(super) body: Option<Buffer>,
    source_rect: Rect,
}

/// Recognize the final framed Claude composer, including multiline input.
/// Never filter by model/status text: those words can occur in real answers.
pub(super) fn composer_start(lines: &[&str]) -> Option<usize> {
    let rule = |line: &str| {
        let line = line.trim();
        !line.is_empty() && line.chars().all(|ch| matches!(ch, '─' | '━'))
    };
    let start = lines.len().saturating_sub(32);
    (start..lines.len().saturating_sub(2)).rev().find(|&top| {
        rule(lines[top])
            && lines[top + 1].trim_start().starts_with('❯')
            && (top + 2..(top + 10).min(lines.len())).any(|bottom| rule(lines[bottom]))
    })
}

impl ClientShellState {
    /// Capture only the focused pane, at most 10 Hz and only after its cells change.
    /// This does no RPC/file I/O and never copies the native editor or status footer.
    pub(super) fn refresh_local_chat_surface(
        &mut self,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) {
        let Some(chat) = self
            .local_chat
            .as_mut()
            .filter(|chat| chat.active && now >= chat.next_refresh)
        else {
            return;
        };
        let Some(surface) = self
            .pane_surface
            .as_ref()
            .filter(|surface| surface.boot_id == chat.boot_id)
        else {
            return;
        };
        let Some(pane) = surface
            .panes
            .iter()
            .find(|pane| pane.pane_id == chat.pane_id)
        else {
            return;
        };
        let frame = &surface.frame;
        if frame.cells.len() != usize::from(frame.width) * usize::from(frame.height) {
            return;
        }
        let rect = Rect::new(
            pane.inner_rect.x,
            pane.inner_rect.y,
            pane.inner_rect.width,
            pane.inner_rect.height,
        )
        .intersection(Rect::new(0, 0, frame.width, frame.height));
        if chat.last_revision == Some(pane.content_revision)
            && chat.live_surface.source_rect == rect
        {
            return;
        }
        chat.last_revision = Some(pane.content_revision);
        chat.next_refresh = now + Duration::from_millis(100);
        chat.live_surface.source_rect = rect;
        let row_cells = |y: u16| {
            let start = usize::from(y) * usize::from(frame.width) + usize::from(rect.x);
            &frame.cells[start..start + usize::from(rect.width)]
        };
        let tail_start = rect.bottom().saturating_sub(32).max(rect.y);
        let tail: Vec<String> = (tail_start..rect.bottom())
            .map(|y| {
                let mut line = String::new();
                for cell in row_cells(y) {
                    line.push_str(&cell.symbol);
                }
                line
            })
            .collect();
        let refs: Vec<&str> = tail.iter().map(String::as_str).collect();
        let body = composer_start(&refs).and_then(|index| {
            let mut bottom = tail_start + index as u16;
            while bottom > rect.y
                && row_cells(bottom - 1)
                    .iter()
                    .all(|cell| cell.symbol.trim().is_empty())
            {
                bottom -= 1;
            }
            if bottom == rect.y || rect.width == 0 {
                return None;
            }
            let mut buffer = Buffer::empty(Rect::new(0, 0, rect.width, bottom - rect.y));
            for y in rect.y..bottom {
                for (x, cell) in row_cells(y).iter().enumerate() {
                    cell.copy_to_ratatui_cell(&mut buffer[(x as u16, y - rect.y)]);
                }
            }
            Some(buffer)
        });
        outcome.repaint |= chat.live_surface.body != body;
        chat.live_surface.body = body;
    }
}

#[cfg(test)]
mod tests {
    use super::composer_start;

    #[test]
    fn final_multiline_composer_wins_over_quoted_prompt_rules() {
        assert_eq!(
            composer_start(&[
                "answer",
                "────",
                "❯ quoted",
                "────",
                "more answer",
                "────",
                "❯ typed",
                "  second line",
                "────",
                "Opus | main",
                "auto mode"
            ]),
            Some(5)
        );
        assert_eq!(
            composer_start(&["answer containing Opus and ctx", "no framed composer"]),
            None
        );
    }
}
