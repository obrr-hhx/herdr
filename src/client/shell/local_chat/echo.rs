//! Local submission feedback, kept separate from saved conversation records.
use super::*;

pub(super) fn saved_prompt_text(saved: &str) -> &str {
    let saved = saved.trim();
    let saved = saved
        .strip_prefix("<pasted_content ")
        .and_then(|wrapped| wrapped.split_once('>'))
        .and_then(|(attributes, body)| {
            let (text, closing) = body.rsplit_once("</pasted_content")?;
            // Claude writes both a plain close tag and a close tag repeating the ID.
            // Require matching attributes so a malformed wrapper cannot confirm a send.
            (closing == ">"
                || closing
                    .strip_prefix(' ')
                    .and_then(|closing| closing.strip_suffix('>'))
                    == Some(attributes))
            .then_some(text.trim())
        })
        .unwrap_or(saved);
    saved
}

fn same_text(saved: &str, submitted: &str) -> bool {
    saved_prompt_text(saved) == submitted.trim()
}

pub(in crate::client::shell) struct LocalEcho {
    pub(super) text: String,
    pub(super) acknowledged: bool,
    after: Option<u64>,
    started: Instant,
}

impl LocalChat {
    pub(super) fn stage_echo(&mut self, text: String) {
        self.echoes.push_back(LocalEcho {
            text,
            acknowledged: false,
            after: self.transcript.records.iter().map(|r| r.offset).max(),
            started: Instant::now(),
        });
        self.echo_dirty = true;
    }

    pub(super) fn acknowledge_echo(&mut self, succeeded: bool) {
        if let Some(index) = self.echoes.iter().rposition(|e| !e.acknowledged) {
            if succeeded {
                let echo = &mut self.echoes[index];
                echo.acknowledged = true;
                tracing::debug!(target:"herdr::client::diagnostics", elapsed_ms=echo.started.elapsed().as_millis() as u64,
                    "local chat submission acknowledged");
            } else {
                self.echoes.remove(index);
            }
        }
        self.echo_dirty = true;
        self.transcript.next = None;
    }

    pub(super) fn confirm_echoes(&mut self, records: &[crate::agent_history::HistoryRecord]) {
        for record in records.iter().filter(|r| r.kind == "user") {
            if let Some(index) = self.echoes.iter().position(|echo| {
                echo.after.is_none_or(|after| record.offset > after)
                    && same_text(&record.text, &echo.text)
            }) {
                if let Some(echo) = self.echoes.remove(index) {
                    tracing::debug!(target:"herdr::client::diagnostics", elapsed_ms=echo.started.elapsed().as_millis() as u64,
                        "local chat submission found in saved history");
                }
                self.echo_dirty = true;
            }
        }
    }
}

impl ClientShellState {
    /// Reuse the received terminal surface; do not queue passive RPCs ahead of a send.
    pub(super) fn refresh_local_chat_surface(
        &mut self,
        now: Instant,
        outcome: &mut ClientShellInput,
    ) {
        let Some(chat) = self
            .local_chat
            .as_mut()
            .filter(|c| c.active && now >= c.next_refresh)
        else {
            return;
        };
        let Some(surface) = self.pane_surface.as_ref() else {
            return;
        };
        let Some(pane) = surface.panes.iter().find(|p| p.pane_id == chat.pane_id) else {
            return;
        };
        if chat.last_revision == Some(pane.content_revision) {
            return;
        }
        chat.last_revision = Some(pane.content_revision);
        chat.next_refresh = now + Duration::from_millis(100);
        let rect = Rect::new(
            pane.inner_rect.x,
            pane.inner_rect.y,
            pane.inner_rect.width,
            pane.inner_rect.height,
        );
        let start = if chat.transcript.loaded {
            rect.bottom().saturating_sub(12).max(rect.y)
        } else {
            rect.y
        };
        let mut lines = Vec::new();
        for y in start..rect.bottom().min(surface.frame.height) {
            let mut line = String::new();
            for x in rect.x..rect.right().min(surface.frame.width) {
                if let Some(cell) = surface
                    .frame
                    .cells
                    .get(usize::from(y) * usize::from(surface.frame.width) + usize::from(x))
                {
                    line.push_str(&cell.symbol);
                }
            }
            if !line.trim().is_empty() {
                lines.push(line.trim_end().to_string());
            }
        }
        if chat.transcript.loaded {
            chat.live_preview = lines.into_iter().rev().take(5).collect();
            chat.live_preview.reverse();
        } else {
            chat.update_text(lines.join("\n"), false);
        }
        outcome.repaint = true;
    }
}

#[cfg(test)]
mod tests {
    use super::same_text;

    #[test]
    fn malformed_paste_wrappers_cannot_confirm_another_message() {
        for saved in [
            "<pasted_content id=\"a\">hello</pasted_content id=\"b\">",
            "<pasted_content id=\"a\">hello</pasted_content>trailing",
            "prefix<pasted_content id=\"a\">hello</pasted_content>",
        ] {
            assert!(!same_text(saved, "hello"));
        }
    }
}
