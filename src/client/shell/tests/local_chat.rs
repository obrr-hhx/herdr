use super::*;
use crate::api::schema::{PaneReadResult, ReadFormat, ReadSource, ResponseResult};

fn press(state: &mut ClientShellState, key: KeyCode, modifiers: KeyModifiers) -> ClientShellInput {
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

fn reader() -> (ClientShellState, String) {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
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

fn history(text: String) -> ResponseResult {
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
