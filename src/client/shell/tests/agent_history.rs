use super::local_chat::{frame_text, history, press, reader};
use super::*;
use crate::agent_history::{HistoryPage, HistoryRecord};
use crate::client::history_jobs::HistoryResult;

fn record(index: u64, kind: &str, text: &str) -> HistoryRecord {
    HistoryRecord {
        id: format!("record-{index:05}"),
        offset: index * 100,
        kind: kind.into(),
        text: text.into(),
    }
}
fn page(records: Vec<HistoryRecord>, before: u64) -> HistoryPage {
    HistoryPage {
        version: 1,
        session_id: "session-a".into(),
        terminal_id: "terminal-a".into(),
        revision: "rev-1".into(),
        records,
        before,
        has_older: before > 0,
        ..HistoryPage::default()
    }
}
fn complete(state: &mut ClientShellState, page: HistoryPage, older: bool) -> bool {
    state.complete_agent_history(HistoryResult {
        endpoint: state.active_endpoint_id.clone(),
        boot: "boot-1".into(),
        generation: state.local_chat_generation,
        older,
        result: Ok(page),
    })
}

#[test]
fn saved_history_includes_early_messages_beyond_screen_and_two_thousand_lines() {
    let (mut state, screen_read) = reader();
    let records = (0..2600)
        .map(|i| record(i, "assistant", &format!("SAVED_MESSAGE_{i:04}")))
        .collect();
    complete(&mut state, page(records, 0), false);
    // A passive screen response arriving late must not wipe the conversation.
    state.handle_endpoint_result(
        "boot-1",
        &screen_read,
        Ok(history("only the last screen".into())),
    );
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    let frame = state.compose(106, 24).unwrap();
    assert!(frame_text(&frame).contains("SAVED_MESSAGE_0000"));
    assert!(state
        .local_chat
        .as_ref()
        .unwrap()
        .text
        .contains("SAVED_MESSAGE_2599"));
}

#[test]
fn saved_history_styles_distinguish_user_code_tools_and_assistant() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(
            vec![
                record(0, "user", "hello"),
                record(1, "assistant", "```rust\nfn main() {}\n```"),
                record(2, "tool_result", "tests passed"),
            ],
            0,
        ),
        false,
    );
    let frame = state.compose(106, 30).unwrap();
    let buffer = frame.to_ratatui_buffer().unwrap();
    let palette = &state.config.palette;
    for color in [palette.green, palette.teal, palette.mauve, palette.accent] {
        assert!(
            buffer
                .content()
                .iter()
                .any(|c| c.fg == color && !c.symbol().trim().is_empty()),
            "missing semantic color {color:?}"
        );
    }
}

#[test]
fn saved_history_does_not_erase_endpoint_notice_content() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(vec![record(0, "assistant", "Saved reply")], 0),
        false,
    );
    state.push_endpoint_notice(
        ClientEndpointNoticeKind::Unavailable,
        "history-test-notice",
        "Connection notice",
        "The endpoint is temporarily unavailable",
    );
    let frame = state.compose(106, 24).unwrap();
    let text = frame_text(&frame);
    assert!(text.contains("Connection notice"), "{text}");
    assert!(
        text.contains("The endpoint is temporarily unavailable"),
        "{text}"
    );
    assert!(text.contains("Saved reply"), "{text}");
}

#[test]
fn saved_history_refresh_deduplicates_records_and_freezes_reading_until_live() {
    let (mut state, _) = reader();
    let records = (0..100)
        .map(|i| record(i, "assistant", &format!("message {i}")))
        .collect();
    complete(&mut state, page(records, 0), false);
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
    let before = frame_text(&state.compose(106, 24).unwrap());
    complete(
        &mut state,
        page(
            vec![
                record(99, "assistant", "updated message"),
                record(100, "assistant", "NEW_SAVED_REPLY"),
            ],
            9000,
        ),
        false,
    );
    let after = frame_text(&state.compose(106, 24).unwrap());
    assert_eq!(
        before.lines().skip(3).take(10).collect::<Vec<_>>(),
        after.lines().skip(3).take(10).collect::<Vec<_>>()
    );
    assert!(!after.contains("NEW_SAVED_REPLY"));
    press(&mut state, KeyCode::End, KeyModifiers::CONTROL);
    let frame = frame_text(&state.compose(106, 24).unwrap());
    assert!(frame.contains("NEW_SAVED_REPLY"));
    assert_eq!(
        state.local_chat.as_ref().unwrap().transcript.records.len(),
        101
    );
}

#[test]
fn saved_history_loads_older_page_without_jumping_or_marking_latest_as_fetched() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(
            (20..40)
                .map(|i| record(i, "assistant", &format!("message {i:02}")))
                .collect(),
            2000,
        ),
        false,
    );
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    assert!(state.local_chat.as_ref().unwrap().transcript.want_older);
    let before = frame_text(&state.compose(106, 24).unwrap());
    let mut old = page(
        (0..20)
            .map(|i| record(i, "assistant", &format!("message {i:02}")))
            .collect(),
        0,
    );
    old.revision = "new-tail-not-fetched".into();
    complete(&mut state, old, true);
    let after = frame_text(&state.compose(106, 24).unwrap());
    assert_eq!(
        before
            .lines()
            .skip(3)
            .take(8)
            .map(|line| line.chars().take(100).collect::<String>())
            .collect::<Vec<_>>(),
        after
            .lines()
            .skip(3)
            .take(8)
            .map(|line| line.chars().take(100).collect::<String>())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        state.local_chat.as_ref().unwrap().transcript.revision,
        "rev-1"
    );
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    assert!(frame_text(&state.compose(106, 24).unwrap()).contains("message 00"));
}

#[test]
fn saved_history_rejects_late_boot_or_generation_and_does_not_mix_claude_sessions() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(vec![record(0, "assistant", "OLD_SESSION_ONLY")], 0),
        false,
    );
    let mut other = page(vec![record(1, "assistant", "NEW_SESSION_ONLY")], 0);
    other.session_id = "session-b".into();
    complete(&mut state, other, false);
    assert!(!state
        .local_chat
        .as_ref()
        .unwrap()
        .text
        .contains("OLD_SESSION_ONLY"));
    let generation = state.local_chat_generation;
    state.interrupt_local_chat();
    assert!(!state.complete_agent_history(HistoryResult {
        endpoint: state.active_endpoint_id.clone(),
        boot: "boot-1".into(),
        generation,
        older: false,
        result: Ok(page(vec![record(2, "assistant", "LATE_REPLY")], 0))
    }));
    assert!(!state
        .local_chat
        .as_ref()
        .unwrap()
        .text
        .contains("LATE_REPLY"));
}

#[test]
fn saved_history_older_request_survives_latest_reply_in_flight_and_keeps_new_records() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(
            (20..40)
                .map(|i| record(i, "assistant", &format!("message {i:02}")))
                .collect(),
            2000,
        ),
        false,
    );
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    // The refresh already in flight completes after the user requested an older page.
    complete(
        &mut state,
        page(
            vec![
                record(39, "assistant", "message 39 updated"),
                record(40, "assistant", "LATEST_AFTER_OLDER"),
            ],
            2000,
        ),
        false,
    );
    assert!(state.local_chat.as_ref().unwrap().transcript.want_older);
    complete(
        &mut state,
        page(
            (0..20)
                .map(|i| record(i, "assistant", &format!("message {i:02}")))
                .collect(),
            0,
        ),
        true,
    );
    let visible = frame_text(&state.compose(106, 24).unwrap());
    assert!(visible.contains("message 20"));
    press(&mut state, KeyCode::End, KeyModifiers::CONTROL);
    assert!(frame_text(&state.compose(106, 24).unwrap()).contains("LATEST_AFTER_OLDER"));
    assert!(state
        .local_chat
        .as_ref()
        .unwrap()
        .text
        .contains("message 00"));
}

#[test]
fn saved_history_live_terminal_tail_is_separate_and_hidden_while_reading() {
    let (mut state, screen_read) = reader();
    complete(
        &mut state,
        page(
            (0..80)
                .map(|i| record(i, "assistant", &format!("saved {i}")))
                .collect(),
            0,
        ),
        false,
    );
    state.snapshot.as_mut().unwrap().agents[0].agent_status =
        crate::api::schema::AgentStatus::Working;
    state.handle_endpoint_result(
        "boot-1",
        &screen_read,
        Ok(history("UNSAVED_LIVE_FRAGMENT".into())),
    );
    let frame = frame_text(&state.compose(106, 30).unwrap());
    assert!(frame.contains("Live terminal"));
    assert!(frame.contains("UNSAVED_LIVE_FRAGMENT"));
    assert!(!state
        .local_chat
        .as_ref()
        .unwrap()
        .text
        .contains("UNSAVED_LIVE_FRAGMENT"));
    press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
    assert!(!frame_text(&state.compose(106, 30).unwrap()).contains("UNSAVED_LIVE_FRAGMENT"));
}

#[test]
fn saved_history_empty_older_page_clears_loading_and_advances_cursor() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(vec![record(20, "assistant", "loaded")], 2000),
        false,
    );
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    assert!(state.local_chat.as_ref().unwrap().transcript.want_older);
    assert!(complete(&mut state, page(vec![], 1000), true));
    assert!(!state.local_chat.as_ref().unwrap().transcript.want_older);
    assert_eq!(state.local_chat.as_ref().unwrap().transcript.before, 1000);
}

#[test]
fn saved_history_blocked_agent_shows_return_to_terminal_action() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(vec![record(0, "assistant", "saved")], 0),
        false,
    );
    state.snapshot.as_mut().unwrap().agents[0].agent_status =
        crate::api::schema::AgentStatus::Blocked;
    assert!(frame_text(&state.compose(106, 24).unwrap()).contains("Needs attention"));
}

#[test]
fn saved_history_non_message_prefix_does_not_invent_an_older_page() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(
            vec![record(20, "assistant", "first real turn after metadata")],
            0,
        ),
        false,
    );
    state.compose(106, 24).unwrap();
    press(&mut state, KeyCode::Home, KeyModifiers::CONTROL);
    assert!(!state.local_chat.as_ref().unwrap().transcript.has_older);
    assert!(!state.local_chat.as_ref().unwrap().transcript.want_older);
}

#[test]
fn saved_history_success_clears_an_interrupted_fallback_notice() {
    let (mut state, screen_read) = reader();
    state.handle_endpoint_result(
        "boot-1",
        &screen_read,
        Err(ClientShellEndpointError {
            code: Some("interrupted".into()),
            message: "This server action was interrupted".into(),
        }),
    );
    complete(
        &mut state,
        page(
            vec![record(0, "assistant", "saved conversation is available")],
            0,
        ),
        false,
    );
    let frame = frame_text(&state.compose(106, 24).unwrap());
    assert!(frame.contains("saved conversation is available"));
    assert!(!frame.contains("History unavailable"));
}

#[test]
#[ignore = "non-gating saved-history input/render profile"]
fn saved_history_render_profile() {
    for count in [1, 15] {
        let (mut state, _) = reader();
        let first = state.snapshot.as_ref().unwrap().panes[0].clone();
        for index in 1..count {
            let mut pane = first.clone();
            pane.pane_id = format!("history-extra-{index}");
            pane.focused = false;
            state.snapshot.as_mut().unwrap().panes.push(pane);
        }
        let records=(0..5000).map(|i|record(i,"assistant",&format!("Saved reply {i}\n中文 content in a real-width editor\n```rust\nlet cached = true;\n```"))).collect();
        let start = std::time::Instant::now();
        complete(&mut state, page(records, 0), false);
        state.compose(120, 40).unwrap();
        let load_us = start.elapsed().as_micros();
        let mut typing = Vec::new();
        let mut scrolling = Vec::new();
        for _ in 0..100 {
            let start = std::time::Instant::now();
            let outcome = state.handle_input_bytes(b"x");
            assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
            std::hint::black_box(state.compose(120, 40).unwrap());
            typing.push(start.elapsed().as_micros());
            let start = std::time::Instant::now();
            let outcome = press(&mut state, KeyCode::PageUp, KeyModifiers::empty());
            assert!(outcome.actions.is_empty() && outcome.requests.is_empty());
            std::hint::black_box(state.compose(120, 40).unwrap());
            scrolling.push(start.elapsed().as_micros());
        }
        typing.sort_unstable();
        scrolling.sort_unstable();
        eprintln!("saved_history panes={count} records=5000 load_us={load_us} typing_p50_us={} typing_p95_us={} scroll_p50_us={} scroll_p95_us={}",typing[50],typing[95],scrolling[50],scrolling[95]);
    }
}

#[test]
fn saved_history_recovery_repaints_even_when_file_is_unchanged() {
    let (mut state, _) = reader();
    complete(
        &mut state,
        page(vec![record(0, "assistant", "saved")], 0),
        false,
    );
    state.local_chat.as_mut().unwrap().status =
        "Saved history unavailable: temporary failure".into();
    let mut same = page(vec![], 0);
    same.unchanged = true;
    assert!(complete(&mut state, same, false));
    assert!(!frame_text(&state.compose(106, 24).unwrap()).contains("unavailable"));
}
