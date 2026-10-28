//! Read-only, client-owned previews. Cached pixels never grant an input lease.
use super::*;
use std::sync::Arc;

const MAX_SCREENS: usize = 8;
const MAX_CELLS: usize = 200_000;

pub(super) struct CachedScreen {
    pub(super) endpoint: ClientEndpointId,
    snapshot: Box<ClientShellSnapshot>,
    visual: Option<FrameData>,
    reader_height: Option<u16>,
}

pub(super) struct ActivationPreview {
    pub(super) endpoint: ClientEndpointId,
    screen: Arc<CachedScreen>,
}

impl ClientShellState {
    pub(crate) fn has_activation_preview(&self) -> bool {
        self.activation_preview.is_some()
    }
    pub(crate) fn remember_endpoint_screen(&mut self, cols: u16, rows: u16) {
        if self.activation_preview.is_some() || self.overlay.is_some() {
            return;
        }
        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };
        if !self.local_chat_visible()
            && self
                .pane_surface
                .as_ref()
                .is_none_or(|s| s.projection_revision != snapshot.revision)
        {
            return;
        }
        let snapshot = snapshot.clone();
        let endpoint = self.active_endpoint_id.clone();
        let area = self.layout(cols, rows).pane_surface;
        let Some(frame) = self.compose(cols, rows) else {
            return;
        };
        let Some(buffer) = frame.to_ratatui_buffer() else {
            return;
        };
        let reader_height = self
            .local_chat
            .as_ref()
            .filter(|_| self.local_chat_visible())
            .map(|chat| chat.editor_area.y.saturating_sub(1).saturating_sub(area.y));
        let mut pane = Buffer::empty(Rect::new(0, 0, area.width, area.height));
        for y in 0..area.height {
            for x in 0..area.width {
                pane[(x, y)] = buffer[(area.x + x, area.y + y)].clone();
            }
        }
        let visual = FrameData::from_ratatui_buffer_with_hyperlinks(&pane, None, &[]);
        tracing::debug!(target:"herdr::client::diagnostics",endpoint=?endpoint,
            reader_height=?reader_height, nonblank=visual.cells.iter().filter(|c|!c.symbol.trim().is_empty()).count(),
            skipped=visual.cells.iter().filter(|c|c.skip).count(),"cached screen captured");
        if visual.cells.len() > MAX_CELLS {
            return;
        }
        self.cached_screens.retain(|s| {
            s.endpoint != endpoint || s.snapshot.focused_pane_id != snapshot.focused_pane_id
        });
        self.cached_screens.push_back(Arc::new(CachedScreen {
            endpoint,
            snapshot,
            visual: Some(visual),
            reader_height,
        }));
        while self.cached_screens.len() > MAX_SCREENS
            || self
                .cached_screens
                .iter()
                .filter_map(|s| s.visual.as_ref())
                .map(|f| f.cells.len())
                .sum::<usize>()
                > MAX_CELLS
        {
            self.cached_screens.pop_front();
        }
    }

    pub(crate) fn start_activation_preview(
        &mut self,
        endpoint: &ClientEndpointId,
        target: Option<&ClientEndpointFocusTarget>,
    ) {
        let Some(snapshot) = self
            .endpoints
            .iter()
            .find(|e| &e.endpoint_id == endpoint)
            .and_then(|e| e.snapshot.as_ref())
        else {
            return;
        };
        let screen = self
            .cached_screens
            .iter()
            .rev()
            .find(|screen| {
                &screen.endpoint == endpoint
                    && screen.snapshot.boot_id == snapshot.boot_id
                    && match target {
                        Some(ClientEndpointFocusTarget::Pane(id)) => {
                            screen.snapshot.focused_pane_id.as_ref() == Some(id)
                        }
                        Some(ClientEndpointFocusTarget::Tab(id)) => {
                            screen.snapshot.focused_tab_id.as_ref() == Some(id)
                        }
                        Some(ClientEndpointFocusTarget::Workspace(id)) => {
                            screen.snapshot.focused_workspace_id.as_ref() == Some(id)
                        }
                        None => true,
                    }
            })
            .cloned()
            .unwrap_or_else(|| {
                Arc::new(CachedScreen {
                    endpoint: endpoint.clone(),
                    snapshot: snapshot.clone(),
                    visual: None,
                    reader_height: None,
                })
            });
        self.activation_preview = Some(ActivationPreview {
            endpoint: endpoint.clone(),
            screen,
        });
    }

    pub(crate) fn clear_activation_preview(&mut self) {
        self.activation_preview = None;
        self.hits = ShellHitMap::default();
    }

    pub(super) fn compose_activation_preview(&mut self, cols: u16, rows: u16) -> Option<FrameData> {
        let preview = self.activation_preview.as_ref()?;
        let endpoint = preview.endpoint.clone();
        let screen = Arc::clone(&preview.screen);
        let layout = self.layout(cols, rows);
        let mut buffer = Buffer::empty(Rect::new(0, 0, cols, rows));
        buffer.set_style(
            buffer.area,
            Style::default()
                .fg(self.config.palette.text)
                .bg(self.config.palette.panel_bg),
        );
        self.hits = render::render_shell(
            &mut buffer,
            layout,
            &screen.snapshot,
            &self.config,
            render::ShellRenderState {
                endpoints: &self.endpoints,
                active_endpoint_id: &endpoint,
                collapsed_endpoints: &self.collapsed_endpoints,
                collapsed_groups: &self.collapsed_groups,
                remote_collapsed_groups: &self.remote_collapsed_groups,
                workspace_scroll: &mut self.workspace_scroll,
                agent_scroll: &mut self.agent_scroll,
                tab_scroll: &mut self.tab_scroll,
                reveal_focused_workspace: &mut self.reveal_focused_workspace,
                reveal_focused_tab: &mut self.reveal_focused_tab,
                sidebar_collapsed: self.sidebar_collapsed,
                sidebar_section_split: self.sidebar_section_split,
                tab_drag_insert_index: None,
                selected_workspace_id: None,
                reveal_navigation_workspace: &mut self.reveal_navigation_workspace,
                dragged_workspace_id: None,
                workspace_drop_indicator_row: None,
            },
        );
        // Only endpoint-qualified sidebar navigation remains interactive while syncing.
        self.hits.panes.clear();
        self.hits.pane_splits.clear();
        self.hits.popup = None;
        self.hits.tabs.clear();
        self.hits.new_tab = Rect::default();
        self.hits.tab_scroll_left = Rect::default();
        self.hits.tab_scroll_right = Rect::default();
        self.hits.new_workspace = Rect::default();
        let mut frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
        if let Some(visual) = &screen.visual {
            let mut area = layout.pane_surface;
            if let Some(height) = screen.reader_height {
                area.height = area.height.saturating_sub(2).min(height);
            }
            blit_pane_surface(&mut frame, visual, area);
        }
        let mut buffer = frame.to_ratatui_buffer()?;
        if !layout.pane_surface.is_empty() {
            let row = Rect::new(
                layout.pane_surface.x,
                layout.pane_surface.y,
                layout.pane_surface.width,
                1,
            );
            ratatui::widgets::Widget::render(ratatui::widgets::Clear, row, &mut buffer);
            buffer.set_style(
                row,
                Style::default()
                    .fg(self.config.palette.text)
                    .bg(self.config.palette.panel_bg),
            );
        }
        if screen.reader_height.is_some() && layout.pane_surface.height > 2 {
            render::put_text(
                &mut buffer,
                layout.pane_surface.x + 1,
                layout.pane_surface.bottom() - 1,
                layout.pane_surface.width.saturating_sub(2),
                "Syncing remote view · keyboard input paused",
                Style::default()
                    .fg(self.config.palette.overlay0)
                    .bg(self.config.palette.panel_bg),
            );
        }
        let label = self.endpoint_label(&endpoint);
        let state = if screen.visual.is_some() {
            "Cached view · syncing · input paused"
        } else {
            "Connecting · waiting for first image · input paused"
        };
        render::put_text(
            &mut buffer,
            layout.pane_surface.x,
            layout.pane_surface.y,
            layout.pane_surface.width,
            &format!("{label} · {state}"),
            Style::default()
                .fg(self.config.palette.yellow)
                .bg(self.config.palette.surface0),
        );
        let mut frame = FrameData::from_ratatui_buffer_with_hyperlinks(&buffer, None, &[]);
        self.compose_graphics(&mut frame, layout);
        Some(frame)
    }
}
