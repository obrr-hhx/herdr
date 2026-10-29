use super::*;
use crate::api::schema::{PaneReadResult, ReadFormat, ReadSource, ResponseResult};

pub(super) fn press(
    state: &mut ClientShellState,
    key: KeyCode,
    modifiers: KeyModifiers,
) -> ClientShellInput {
    state.handle_raw_events(vec![RawInputEvent::Key(crate::input::TerminalKey::new(
        key, modifiers,
    ))])
}

fn request(actions: &[ClientShellAction]) -> &crate::api::schema::Request {
    let [ClientShellAction::Endpoint { request, .. }] = actions else {
        panic!("expected endpoint request")
    };
    request
}

pub(super) fn reader() -> (ClientShellState, String) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.history_source_enabled = false; // This fixture injects terminal reads; transcript tests opt in explicitly.
    let mut snap = snapshot();
    snap.agents.push(ClientShellAgent {
        pane_id: "pane_1".into(),
        workspace_id: "ws_1".into(),
        tab_id: "tab_1".into(),
        name: None,
        display_agent: Some("Claude".into()),
        agent: Some("claude".into()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        agent_status: AgentStatus::Idle,
        state_change_seq: 0,
        state_labels: Vec::new(),
        tokens: Vec::new(),
        focused: true,
    });
    state.set_snapshot(Box::new(snap));
    state.set_pane_surface(surface());
    let open = press(&mut state, KeyCode::F(6), KeyModifiers::empty());
    let id = request(&open.actions).id.clone();
    assert!(matches!(
        request(&open.actions).method,
        crate::api::schema::Method::PaneRead(_)
    ));
    state.compose(106, 24).expect("local view");
    (state, id)
}

pub(super) fn history(text: String) -> ResponseResult {
    ResponseResult::PaneRead {
        read: PaneReadResult {
            pane_id: "pane_1".into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            source: ReadSource::RecentUnwrapped,
            format: ReadFormat::Text,
            text,
            revision: 1,
            truncated: false,
        },
    }
}

#[test]
fn local_chat_typing_does_not_wait_for_remote_history_or_acknowledgements() {
    let (mut state, _) = reader();
    // Keep the remote request unresolved: editing and rendering must still complete locally.
    let edit = state.handle_raw_events(vec![RawInputEvent::Text(crate::input::TextCommit::new(
        "你好 world",
    ))]);
    assert!(edit.actions.is_empty() && edit.requests.is_empty() && edit.repaint);
    press(&mut state, KeyCode::Left, KeyModifiers::empty());
    press(&mut state, KeyCode::Backspace, KeyModifiers::empty());
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "你好 word");
    assert!(state
        .compose(106, 24)
        .unwrap()
        .cells
        .iter()
        .any(|c| c.symbol == "你"));
    let submit = press(&mut state, KeyCode::Enter, KeyModifiers::empty());
    assert!(
        matches!(&request(&submit.actions).method, crate::api::schema::Method::AgentPrompt(p) if p.text=="你好 word" && p.target=="pane_1")
    );
    state.handle_input_bytes(b"next draft");
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "next draft");
    assert!(press(&mut state, KeyCode::Enter, KeyModifiers::empty())
        .actions
        .is_empty());
}

#[test]
fn local_chat_scroll_and_copy_use_cache_without_remote_requests() {
    let (mut state, id) = reader();
    let text = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    state.handle_endpoint_result("boot-1", &id, Ok(history(text.clone())));
    state.compose(106, 24).unwrap();
    let up = press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
    assert!(up.requests.is_empty() && up.actions.is_empty());
    assert!(state.local_chat.as_ref().unwrap().offset > 0);
    let copy = press(&mut state, KeyCode::Char('y'), KeyModifiers::CONTROL);
    assert!(
        matches!(&copy.actions[..], [ClientShellAction::ClipboardWrite(bytes)] if bytes==text.as_bytes())
    );
}

#[test]
fn local_chat_uncertain_send_preserves_both_drafts_and_never_auto_retries() {
    let (mut state, _) = reader();
    state.handle_input_bytes(b"first");
    let sent = press(&mut state, KeyCode::Enter, KeyModifiers::empty());
    state.handle_input_bytes(b"second");
    state.handle_endpoint_result(
        "boot-1",
        &request(&sent.actions).id,
        Err(ClientShellEndpointError {
            code: Some("endpoint_timeout".into()),
            message: "timeout".into(),
        }),
    );
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "first\nsecond");
    assert!(press(&mut state, KeyCode::Enter, KeyModifiers::empty())
        .actions
        .is_empty());
    press(&mut state, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(matches!(
        request(&press(&mut state, KeyCode::Enter, KeyModifiers::empty()).actions).method,
        crate::api::schema::Method::AgentPrompt(_)
    ));
}

#[test]
fn local_chat_reading_snapshot_stays_fixed_until_returning_to_live() {
    let (mut state, id) = reader();
    let text = (0..100).map(|n| format!("line {n}\n")).collect::<String>();
    state.handle_endpoint_result("boot-1", &id, Ok(history(text.clone())));
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
    let before = state.local_chat.as_ref().unwrap().offset;
    let mut outcome = ClientShellInput::default();
    state.pane_surface.as_mut().unwrap().panes[0].content_revision += 1;
    state.tick_local_chat(
        std::time::Instant::now() + std::time::Duration::from_secs(1),
        &mut outcome,
    );
    state.handle_endpoint_result(
        "boot-1",
        &request(&outcome.actions).id,
        Ok(history(format!("{text}new output\n"))),
    );
    assert_eq!(state.local_chat.as_ref().unwrap().text, text);
    assert_eq!(state.local_chat.as_ref().unwrap().offset, before);
    press(&mut state, KeyCode::End, KeyModifiers::CONTROL);
    assert!(state
        .local_chat
        .as_ref()
        .unwrap()
        .text
        .contains("new output"));
}

#[test]
fn local_chat_escape_restores_terminal_and_retains_multiline_unicode_draft() {
    let (mut state, _) = reader();
    state.handle_raw_events(vec![RawInputEvent::Paste("中文\r\nhello".into())]);
    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    assert!(!state.local_chat_visible());
    press(&mut state, KeyCode::F(6), KeyModifiers::empty());
    assert!(state.local_chat_visible());
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "中文\nhello");
}

#[test]
fn local_chat_disconnect_recovers_submission_and_fences_late_replies() {
    let (mut state, id) = reader();
    state.handle_input_bytes(b"do once");
    press(&mut state, KeyCode::Enter, KeyModifiers::empty());
    state.interrupt_local_chat();
    state.handle_endpoint_result("boot-1", &id, Ok(history("late cache".into())));
    assert!(state.local_chat.as_ref().unwrap().text.is_empty());
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "do once");
    assert!(press(&mut state, KeyCode::Enter, KeyModifiers::empty())
        .actions
        .is_empty());
}

#[test]
fn local_chat_non_claude_panes_are_not_prompt_targets() {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot()));
    let outcome = press(&mut state, KeyCode::F(6), KeyModifiers::empty());
    assert!(state.local_chat.is_none() && outcome.actions.is_empty());
    assert!(!outcome.requests.is_empty());
}

#[test]
fn local_chat_cached_reader_and_draft_render_while_projection_is_missing() {
    let (mut state, id) = reader();
    state.handle_endpoint_result("boot-1", &id, Ok(history("cached line".into())));
    state.pane_surface = None;
    let text = state.handle_input_bytes(b"offline draft");
    assert!(text.actions.is_empty() && text.requests.is_empty());
    let frame = state.compose(106, 24).expect("cached reader stays usable");
    assert!(frame.cells.iter().any(|c| c.symbol == "o"));
}

#[test]
fn local_chat_restart_requires_explicit_rebind_before_sending() {
    let (mut state, _) = reader();
    state.handle_input_bytes(b"keep my draft");
    state.interrupt_local_chat();
    state.snapshot.as_mut().unwrap().boot_id = "boot-2".into();
    assert!(state.local_chat_visible());
    assert!(press(&mut state, KeyCode::Enter, KeyModifiers::empty())
        .actions
        .is_empty());
    press(&mut state, KeyCode::Char('r'), KeyModifiers::CONTROL);
    assert!(matches!(
        request(&press(&mut state, KeyCode::Enter, KeyModifiers::empty()).actions).method,
        crate::api::schema::Method::AgentPrompt(_)
    ));
}

#[test]
fn local_chat_draft_is_not_forwarded_to_a_different_focused_pane() {
    let (mut state, _) = reader();
    state.handle_input_bytes(b"private draft");
    state.snapshot.as_mut().unwrap().focused_pane_id = Some("other-pane".into());
    assert!(!state.local_chat_visible());
    let open = press(&mut state, KeyCode::F(6), KeyModifiers::empty());
    assert!(open.actions.is_empty());
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "private draft");
}

#[test]
fn local_chat_memory_limits_keep_utf8_and_reject_oversized_paste_atomically() {
    let (mut state, id) = reader();
    state.handle_input_bytes(b"kept");
    state.handle_raw_events(vec![RawInputEvent::Paste("中".repeat(30_000))]);
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "kept");
    state.handle_endpoint_result("boot-1", &id, Ok(history("中".repeat(400_000))));
    let text = &state.local_chat.as_ref().unwrap().text;
    assert!(text.len() <= 1024 * 1024);
    assert!(text.chars().all(|c| c == '中'));
}

#[test]
fn local_chat_releases_terminal_control_and_filters_pasted_escape_sequences() {
    let (mut state, _) = reader();
    let pasted =
        state.handle_raw_events(vec![RawInputEvent::Paste("hello\x1b\r\nworld\x00".into())]);
    assert!(pasted.requests.is_empty());
    assert_eq!(state.local_chat.as_ref().unwrap().draft, "hello\nworld");
    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    let normal = state.handle_input_bytes(b"x");
    assert!(!normal.requests.is_empty());
}

#[test]
fn local_chat_old_server_capabilities_fail_closed_without_sending_a_prompt() {
    let (mut state, _) = reader();
    state.local_chat = None;
    state.set_endpoint_methods(Some(vec!["pane.scroll".into()]));
    let outcome = press(&mut state, KeyCode::F(6), KeyModifiers::empty());
    assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
    assert!(state.local_chat.is_none());
    assert!(state
        .endpoint_error
        .as_deref()
        .unwrap()
        .contains("preview server"));
}

#[test]
#[ignore = "non-gating local input/render profile; no network timing assertion"]
fn local_chat_latency_profile_with_delayed_ack() {
    for count in [1, 15] {
        let (mut state, id) = reader();
        let first = state.snapshot.as_ref().unwrap().panes[0].clone();
        for n in 1..count {
            let mut pane = first.clone();
            pane.pane_id = format!("extra-{n}");
            pane.focused = false;
            state.snapshot.as_mut().unwrap().panes.push(pane);
        }
        let text = (0..2000)
            .map(|n| format!("Cached output row {n}\n"))
            .collect::<String>();
        state.handle_endpoint_result("boot-1", &id, Ok(history(text)));
        state.compose(120, 40).unwrap();
        let mut typing = Vec::new();
        let mut scrolling = Vec::new();
        for _ in 0..100 {
            let t = std::time::Instant::now();
            let event = state.handle_input_bytes(b"x");
            assert!(event.actions.is_empty() && event.requests.is_empty());
            std::hint::black_box(state.compose(120, 40).unwrap());
            typing.push(t.elapsed().as_micros());
            let t = std::time::Instant::now();
            let event = press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
            assert!(event.actions.is_empty() && event.requests.is_empty());
            std::hint::black_box(state.compose(120, 40).unwrap());
            scrolling.push(t.elapsed().as_micros());
        }
        let sent = press(&mut state, KeyCode::Enter, KeyModifiers::empty());
        assert!(matches!(
            request(&sent.actions).method,
            crate::api::schema::Method::AgentPrompt(_)
        ));
        // A 200 ms unacknowledged remote send must not hold the editor lock or queue its keys.
        std::thread::sleep(std::time::Duration::from_millis(200));
        let event = state.handle_input_bytes(b"still editable");
        assert!(event.actions.is_empty());
        assert_eq!(state.local_chat.as_ref().unwrap().draft, "still editable");
        typing.sort_unstable();
        scrolling.sort_unstable();
        eprintln!("local_chat panes={count} cached_lines=2000 typing_p50_us={} typing_p95_us={} scroll_p50_us={} scroll_p95_us={} delayed_ack_ms=200",typing[50],typing[95],scrolling[50],scrolling[95]);
    }
}

pub(super) fn frame_text(frame: &FrameData) -> String {
    frame
        .cells
        .chunks(usize::from(frame.width))
        .map(|row| row.iter().map(|c| c.symbol.as_str()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn local_chat_responsive_editor_cursor_and_help_fit_all_host_sizes() {
    let (mut state, id) = reader();
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(history(
            (0..150)
                .map(|n| format!("Row {n}: 中文 output\n"))
                .collect(),
        )),
    );
    for (cols, rows) in [
        (160, 50),
        (106, 24),
        (80, 24),
        (60, 18),
        (40, 12),
        (30, 9),
        (20, 7),
        (12, 5),
        (1, 1),
        (160, 50),
    ] {
        let frame = state.compose(cols, rows).expect("responsive frame");
        assert_eq!(frame.cells.len(), usize::from(cols) * usize::from(rows));
        if let Some(cursor) = frame.cursor.as_ref() {
            assert!(
                cursor.x < cols && cursor.y < rows,
                "cursor outside {cols}x{rows}"
            );
        }
        let chat = state.local_chat.as_ref().unwrap();
        if chat.area.width >= 16 && chat.area.height >= 7 {
            let text = frame_text(&frame);
            assert!(
                text.contains("❯") && text.contains("──"),
                "editor must be visibly separated at {cols}x{rows}"
            );
            assert!(
                text.contains("Esc"),
                "escape must remain visible at {cols}x{rows}"
            );
            assert_eq!(chat.editor_area.intersection(chat.reader_area).area(), 0);
        }
    }
}

#[test]
fn local_chat_exact_width_cursor_moves_to_next_row_and_click_edits_in_place() {
    let (mut state, _) = reader();
    state.compose(106, 24).unwrap();
    let width = state.local_chat.as_ref().unwrap().width;
    state.handle_input_bytes("x".repeat(usize::from(width)).as_bytes());
    let frame = state.compose(106, 24).unwrap();
    let chat = state.local_chat.as_ref().unwrap();
    let cursor = frame.cursor.unwrap();
    assert_eq!(
        cursor.x, chat.editor_area.x,
        "insertion at wrap boundary belongs to next row"
    );
    let area = chat.editor_area;
    state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: area.x + 2,
        row: area.y,
        modifiers: KeyModifiers::empty(),
    })]);
    state.handle_input_bytes(b"Y");
    assert!(state.local_chat.as_ref().unwrap().draft.starts_with("xxY"));
}

#[test]
fn local_chat_prefix_controls_remain_available() {
    let (mut state, _) = reader();
    // Use the actual configured prefix; the default in this isolated fixture is Ctrl+B.
    press(&mut state, KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(state.mode, ClientShellMode::Prefix);
    assert!(!state.local_chat_visible());
    press(&mut state, KeyCode::Esc, KeyModifiers::empty());
    assert!(state.local_chat_visible());
}

#[test]
fn local_chat_only_covers_focused_split_and_keeps_tab_bar() {
    let (mut state, _) = reader();
    let surface = state.pane_surface.as_mut().unwrap();
    surface.frame = FrameData::from_ratatui_buffer_with_hyperlinks(
        &Buffer::with_lines(["LEFT                     NEIGHBOUR"]),
        None,
        &[],
    );
    let mut other = surface.panes[0].clone();
    other.pane_id = "neighbour".into();
    other.focused = false;
    other.inner_rect = SurfaceRect {
        x: 40,
        y: 1,
        width: 39,
        height: 22,
    };
    surface.panes[0].inner_rect = SurfaceRect {
        x: 0,
        y: 1,
        width: 38,
        height: 22,
    };
    surface.panes.push(other);
    let layout = state.layout(106, 24);
    let frame = state.compose(106, 24).unwrap();
    let area = state.local_chat.as_ref().unwrap().area;
    assert_eq!(area.x, layout.pane_surface.x);
    assert_eq!(area.width, 38);
    assert_eq!(area.y, layout.pane_surface.y + 1);
    assert!(
        frame_text(&frame).contains("NEIGHBOUR"),
        "adjacent terminal must remain visible"
    );
    assert!(
        !state.hits.panes.is_empty(),
        "pane focus/resize targets must survive local view"
    );
}

#[test]
fn local_chat_mouse_wheel_over_editor_does_not_scroll_history_or_send_remote_input() {
    let (mut state, id) = reader();
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(history((0..100).map(|n| format!("line {n}\n")).collect())),
    );
    state.compose(106, 24).unwrap();
    let area = state.local_chat.as_ref().unwrap().editor_area;
    let event = state.handle_raw_events(vec![RawInputEvent::Mouse(MouseEvent {
        kind: crossterm::event::MouseEventKind::ScrollUp,
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::empty(),
    })]);
    assert_eq!(state.local_chat.as_ref().unwrap().offset, 0);
    assert!(event.actions.is_empty() && event.requests.is_empty());
}

#[test]
fn local_chat_history_drops_only_empty_claude_input_footer() {
    let (mut state, id) = reader();
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(history(
            "Answer stays\n\n─────────\n❯\n─────────\nSonnet | main\n".into(),
        )),
    );
    assert_eq!(state.local_chat.as_ref().unwrap().text, "Answer stays");
}

#[test]
fn local_chat_resize_keeps_reading_anchor_and_retained_neighbour_during_projection_gap() {
    let (mut state, id) = reader();
    state.handle_endpoint_result(
        "boot-1",
        &id,
        Ok(history((0..200).map(|n| format!("row {n:03}\n")).collect())),
    );
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
    let before = state.compose(106, 24).unwrap();
    let chat = state.local_chat.as_ref().unwrap();
    let top = usize::from(chat.reader_area.y) * usize::from(before.width)
        + usize::from(chat.reader_area.x);
    let anchor = before.cells[top..top + 7]
        .iter()
        .map(|c| c.symbol.as_str())
        .collect::<String>();
    let after = state.compose(106, 40).unwrap();
    let chat = state.local_chat.as_ref().unwrap();
    let top = usize::from(chat.reader_area.y) * usize::from(after.width)
        + usize::from(chat.reader_area.x);
    assert_eq!(
        anchor,
        after.cells[top..top + 7]
            .iter()
            .map(|c| c.symbol.as_str())
            .collect::<String>()
    );
    // A retained surface must stay visible while the next snapshot is pending.
    state.snapshot.as_mut().unwrap().revision += 1;
    let frame = state
        .compose(106, 40)
        .expect("local cache remains responsive");
    assert!(frame_text(&frame).contains("row "));
    assert!(state.hits.panes.is_empty());
}

#[test]
fn local_chat_overlapping_alternate_screen_windows_retain_real_older_output() {
    let (mut state, id) = reader();
    let rows = |range: std::ops::Range<usize>| {
        range
            .map(|n| format!("CACHE_ROW_{n:03} actual terminal output\n"))
            .collect::<String>()
    };
    state.handle_endpoint_result("boot-1", &id, Ok(history(rows(0..40))));
    state.compose(106, 24).unwrap();
    for (index, range) in [20..60, 40..80, 60..100].into_iter().enumerate() {
        let mut outcome = ClientShellInput::default();
        state.pane_surface.as_mut().unwrap().panes[0].content_revision += 1;
        state.tick_local_chat(
            std::time::Instant::now() + std::time::Duration::from_secs(10 + index as u64),
            &mut outcome,
        );
        state.handle_endpoint_result(
            "boot-1",
            &request(&outcome.actions).id,
            Ok(history(rows(range))),
        );
    }
    let text = &state.local_chat.as_ref().unwrap().text;
    assert_eq!(text, &rows(0..100));
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    let frame = state.compose(106, 24).unwrap();
    assert!(frame_text(&frame).contains("CACHE_ROW_000"));
}

#[test]
fn local_chat_mouse_click_in_neighbour_dispatches_focus_without_terminal_input() {
    let (mut state, _) = reader();
    let layout = state.layout(106, 24);
    let surface = state.pane_surface.as_mut().unwrap();
    let mut other = surface.panes[0].clone();
    other.pane_id = "other".into();
    other.focused = false;
    other.rect = SurfaceRect {
        x: 40,
        y: 1,
        width: 38,
        height: 22,
    };
    other.inner_rect = other.rect;
    surface.panes[0].inner_rect = SurfaceRect {
        x: 0,
        y: 1,
        width: 38,
        height: 22,
    };
    surface.panes.push(other);
    state.compose(106, 24).unwrap();
    let input = format!("\x1b[<0;{};3M", layout.pane_surface.x + 42);
    let outcome = state.handle_input_bytes(input.as_bytes());
    assert!(outcome.actions.iter().any(|action|matches!(action,ClientShellAction::Endpoint{request,..} if matches!(&request.method,crate::api::schema::Method::PaneFocus(target) if target.pane_id=="other"))));
    assert!(outcome.requests.is_empty());
}

#[test]
fn local_chat_vertical_cursor_moves_between_wrapped_visual_rows() {
    let (mut state, _) = reader();
    state.compose(60, 20).unwrap();
    let width = state.local_chat.as_ref().unwrap().width as usize;
    state.handle_input_bytes("x".repeat(width + 4).as_bytes());
    state.compose(60, 20).unwrap();
    press(&mut state, KeyCode::Up, KeyModifiers::empty());
    state.handle_input_bytes(b"Y");
    assert!(state
        .local_chat
        .as_ref()
        .unwrap()
        .draft
        .starts_with("xxxxY"));
}

#[test]
fn local_chat_restart_lock_exposes_recovery_in_the_editor_at_narrow_widths() {
    for cols in [54, 80, 106, 160] {
        let (mut state, _) = reader();
        state.handle_input_bytes(b"never sent draft");
        state.interrupt_local_chat();
        state.snapshot.as_mut().unwrap().boot_id = "boot-2".into();
        let outcome = press(&mut state, KeyCode::Enter, KeyModifiers::empty());
        assert!(outcome.actions.is_empty());
        let frame = state.compose(cols, 24).unwrap();
        let chat = state.local_chat.as_ref().unwrap();
        let title_y = chat.editor_area.y - 1;
        let title: String = frame
            .to_ratatui_buffer()
            .unwrap()
            .content()
            .chunks(usize::from(cols))
            .nth(usize::from(title_y))
            .unwrap()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(title.contains("Ctrl+R"), "cols={cols}: {title}");
        assert_eq!(chat.draft, "never sent draft");
        press(&mut state, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(state.local_chat.as_ref().unwrap().draft, "never sent draft");
        assert!(!press(&mut state, KeyCode::Enter, KeyModifiers::empty())
            .actions
            .is_empty());
    }
}
