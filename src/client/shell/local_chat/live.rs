//! Latest native terminal body. Saved transcript paging remains a separate reader.
use super::*;
use ratatui::buffer::{Buffer, Cell};

type Row = Vec<Cell>;
const MAX_RETAINED_CELLS: usize = 200_000;

#[derive(Default)]
pub(super) struct LiveSurface {
    pub(super) body: Option<Buffer>,
    source_rect: Rect,
    retained: Vec<Row>,
    frozen: Option<Vec<Row>>,
    pub(super) saved_prefix: usize,
}

impl LiveSurface {
    fn rows(&self) -> Vec<Row> {
        let mut rows = self.retained.clone();
        if let Some(body) = &self.body {
            rows.extend(
                body.content
                    .chunks(usize::from(body.area.width))
                    .map(<[Cell]>::to_vec),
            );
        }
        rows
    }

    pub(super) fn freeze(&mut self, saved: &[String]) {
        if self.frozen.is_none() {
            let rows = self.rows();
            // Join persisted history only at a verified text overlap. Native rows
            // remain untouched, so the first scroll cannot replace the visible scene.
            let normal = |line: &str| line.trim().trim_start_matches(['⏺', '❯']).trim().to_owned();
            let saved_keys: Vec<_> = saved.iter().map(|line| normal(line)).collect();
            let keys: Vec<_> = rows
                .iter()
                .map(|row| normal(&row.iter().map(Cell::symbol).collect::<String>()))
                .collect();
            let anchor = keys
                .iter()
                .enumerate()
                .take(32)
                .find_map(|(native_index, key)| {
                    if key.chars().count() < 12 {
                        return None;
                    }
                    saved_keys
                        .iter()
                        .rposition(|value| value == key)
                        .map(|saved_index| (native_index, saved_index))
                });
            if let Some((native_index, saved_index)) = anchor {
                self.saved_prefix = saved_index;
                self.frozen = Some(rows.into_iter().skip(native_index).collect());
            } else {
                self.frozen = Some(rows);
            }
        }
    }

    pub(super) fn reanchor(&mut self, saved: &[String]) {
        if let Some(first) = self.frozen.as_ref().and_then(|rows| rows.first()) {
            let key = first.iter().map(Cell::symbol).collect::<String>();
            let normal = |line: &str| line.trim().trim_start_matches(['⏺', '❯']).trim().to_owned();
            if let Some(index) = saved.iter().rposition(|line| normal(line) == normal(&key)) {
                self.saved_prefix = index;
            }
        }
    }

    pub(super) fn resume(&mut self) {
        self.frozen = None;
        self.saved_prefix = 0;
    }

    pub(super) fn row_count(&self) -> usize {
        self.frozen.as_ref().map_or_else(
            || {
                self.retained.len()
                    + self
                        .body
                        .as_ref()
                        .map_or(0, |body| usize::from(body.area.height))
            },
            |rows| self.saved_prefix + rows.len(),
        )
    }

    pub(super) fn row(&self, index: usize) -> Option<&[Cell]> {
        if let Some(rows) = &self.frozen {
            return index
                .checked_sub(self.saved_prefix)
                .and_then(|index| rows.get(index))
                .map(Vec::as_slice);
        }
        if index < self.retained.len() {
            return self.retained.get(index).map(Vec::as_slice);
        }
        let body = self.body.as_ref()?;
        let start = (index - self.retained.len()).checked_mul(usize::from(body.area.width))?;
        body.content
            .get(start..start + usize::from(body.area.width))
    }

    fn update(&mut self, body: Buffer) {
        if let Some(previous) = &self.body {
            if previous.area.width == body.area.width {
                let width = usize::from(body.area.width);
                let old: Vec<_> = previous.content.chunks(width).collect();
                let new: Vec<_> = body.content.chunks(width).collect();
                // Only retain rows demonstrably scrolled out of the native viewport.
                // Ignore the changing response tail and require two nonblank matching rows.
                let shifted = (1..old.len()).find(|&shift| {
                    let overlap = (old.len() - shift).min(new.len());
                    let matched = old[shift..]
                        .iter()
                        .zip(&new)
                        .take(overlap)
                        .take_while(|(a, b)| a == b)
                        .count();
                    matched >= 2
                        && old[shift..shift + matched]
                            .iter()
                            .filter(|row| row.iter().any(|cell| !cell.symbol().trim().is_empty()))
                            .count()
                            >= 2
                });
                // An unchanged first row means an in-place redraw, not a scroll.
                if old.first() != new.first() {
                    if let Some(shift) = shifted {
                        self.retained
                            .extend(old[..shift].iter().map(|row| row.to_vec()));
                    }
                }
            } else {
                // Old native line wrapping is invalid after a resize. Frozen reading
                // remains stable until the reader explicitly returns to live output.
                self.retained.clear();
            }
        }
        let max_rows = MAX_RETAINED_CELLS / usize::from(body.area.width).max(1);
        if self.retained.len() > max_rows {
            self.retained.drain(..self.retained.len() - max_rows);
        }
        self.body = Some(body);
    }
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
        // Brief redraws without a recognisable footer retain the last coherent
        // native scene instead of flashing the separately formatted saved transcript.
        if let Some(body) = body {
            if chat.echoes.iter().any(|echo| {
                !echo.native_seen && echo.surface_revision != Some(pane.content_revision)
            }) {
                let compact = |text: &str| {
                    text.chars()
                        .filter(|ch| !ch.is_whitespace())
                        .collect::<String>()
                };
                let text = compact(&body.content.iter().map(Cell::symbol).collect::<String>());
                for echo in &mut chat.echoes {
                    if !echo.native_seen
                        && echo.surface_revision != Some(pane.content_revision)
                        && text.contains(&compact(&echo.text))
                    {
                        echo.native_seen = true;
                        chat.echo_dirty = true;
                    }
                }
            }
            outcome.repaint |= chat.live_surface.body.as_ref() != Some(&body);
            chat.live_surface.update(body);
        }
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

#[cfg(test)]
mod continuity_tests {
    use super::*;

    #[test]
    fn native_scroll_retains_rows_and_reading_freezes_cells_until_resume() {
        let mut live = LiveSurface::default();
        live.update(Buffer::with_lines([
            "row one",
            "row two",
            "row three",
            "row four",
        ]));
        live.update(Buffer::with_lines([
            "row two",
            "row three",
            "row four",
            "row five",
        ]));
        assert_eq!(live.row_count(), 5);
        assert_eq!(
            live.row(0)
                .unwrap()
                .iter()
                .map(Cell::symbol)
                .collect::<String>()
                .trim(),
            "row one"
        );
        live.freeze(&[]);
        let frozen = live.frozen.clone();
        live.update(Buffer::with_lines([
            "row three",
            "row four",
            "row five",
            "row six",
        ]));
        assert_eq!(live.frozen, frozen);
        assert_eq!(live.row_count(), 5);
        live.resume();
        assert_eq!(live.row_count(), 6);
    }

    #[test]
    fn saved_history_joins_at_the_latest_matching_native_row_without_duplicating_the_visible_tail()
    {
        let mut live = LiveSurface::default();
        live.update(Buffer::with_lines([
            "native answer line alpha",
            "native answer line beta",
        ]));
        let saved = vec![
            "older answer".into(),
            "⏺ native answer line alpha".into(),
            "native answer line beta".into(),
        ];
        live.freeze(&saved);
        assert_eq!(live.saved_prefix, 1);
        assert_eq!(live.row_count(), 3);
        assert!(live.row(0).is_none());
        assert!(live.row(1).is_some());
        let mut older = vec!["even earlier".into()];
        older.extend(saved);
        live.reanchor(&older);
        assert_eq!(live.saved_prefix, 2);
        assert_eq!(live.row_count(), 4);
    }
    #[test]
    fn native_retention_is_bounded() {
        let mut live = LiveSurface {
            retained: vec![vec![Cell::default(); 100]; MAX_RETAINED_CELLS / 100 + 10],
            ..LiveSurface::default()
        };
        live.update(Buffer::with_lines(["x".repeat(100)]));
        assert_eq!(live.retained.len(), MAX_RETAINED_CELLS / 100);
    }
}
