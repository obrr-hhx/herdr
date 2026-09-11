use super::*;

fn endpoint() -> ClientEndpointId {
    ClientEndpointId::Ssh(
        super::super::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
    )
}

fn lease(id: ClientEndpointId, generation: u64, boot: &str) -> EndpointLease {
    EndpointLease {
        endpoint_id: id,
        generation,
        boot_id: boot.into(),
        minimum_revision: 0,
    }
}

#[derive(Clone)]
struct FakeTransport {
    sent: std::sync::Arc<std::sync::Mutex<Vec<crate::protocol::ClientMessage>>>,
    fail_after_write: bool,
}

impl super::super::EndpointTransport for FakeTransport {
    fn send(&mut self, message: &crate::protocol::ClientMessage) -> std::io::Result<()> {
        self.sent
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(message.clone());
        if self.fail_after_write {
            Err(std::io::Error::other("simulated observed write failure"))
        } else {
            Ok(())
        }
    }
}

fn negotiation() -> super::super::EndpointNegotiation {
    super::super::EndpointNegotiation::new(
        vec!["client_shell.surface.set".into()],
        vec![
            crate::protocol::endpoint::SURFACE_INTEREST_CAPABILITY.into(),
            crate::protocol::endpoint::PRESENTATION_EFFECTS_FENCE_CAPABILITY.into(),
        ],
    )
}

fn test_snapshot(boot_id: &str, revision: u64) -> crate::protocol::ClientShellSnapshot {
    crate::protocol::ClientShellSnapshot {
        boot_id: boot_id.into(),
        revision,
        config_diagnostic: None,
        product_announcement: None,
        update_available: None,
        update_install_command: String::new(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: String::new(),
        release_notes: None,
        focused_workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: String::new(),
        agent_view_label: None,
        agent_order: Vec::new(),
        workspaces: Vec::new(),
        tabs: Vec::new(),
        panes: Vec::new(),
        agents: Vec::new(),
        commands: Vec::new(),
    }
}

type SentMessages = std::sync::Arc<std::sync::Mutex<Vec<crate::protocol::ClientMessage>>>;
type TestFixture = (
    crate::client::ClientShellState,
    EndpointRegistry,
    SentMessages,
    SentMessages,
);

fn shell_and_registry() -> TestFixture {
    shell_and_registry_with_source_failure(false)
}

fn shell_and_registry_with_source_failure(source_fail_after_write: bool) -> TestFixture {
    let mut shell = crate::client::ClientShellState::new(
        crate::client::ClientShellConfig::from_config(&crate::config::Config::default()),
    );
    let profile = super::super::SavedSshEndpoint {
        id: super::super::ProfileId::parse("0123456789abcdef0123456789abcdef").unwrap(),
        label: "Remote".into(),
        target: "dev@example.com".into(),
        session: "main".into(),
        enabled: true,
    };
    let target = ClientEndpointId::Ssh(profile.id.clone());
    shell.set_endpoint_catalog(&[profile]);
    shell.set_snapshot(Box::new(test_snapshot("local-boot", 1)));
    shell.set_endpoint_status(&target, ClientEndpointStatus::Online);
    shell.set_endpoint_snapshot(&target, Box::new(test_snapshot("remote-boot", 1)));

    let local_sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let remote_sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut endpoints = EndpointRegistry::new(
        FakeTransport {
            sent: local_sent.clone(),
            fail_after_write: source_fail_after_write,
        },
        1,
        negotiation(),
    );
    endpoints.insert(
        target,
        FakeTransport {
            sent: remote_sent.clone(),
            fail_after_write: false,
        },
        7,
        negotiation(),
        false,
    );
    (shell, endpoints, local_sent, remote_sent)
}

fn surface_success(id: &str, active: bool, projection_revision: u64) -> Vec<u8> {
    serde_json::to_vec(&crate::api::schema::SuccessResponse {
        id: id.into(),
        result: crate::api::schema::ResponseResult::ClientShellSurfaceSet {
            active,
            projection_revision,
        },
    })
    .unwrap()
}

fn workspace_focus_success(id: &str, workspace_id: &str) -> Vec<u8> {
    serde_json::to_vec(&crate::api::schema::SuccessResponse {
        id: id.into(),
        result: crate::api::schema::ResponseResult::WorkspaceInfo {
            workspace: crate::api::schema::WorkspaceInfo {
                workspace_id: workspace_id.into(),
                number: 1,
                label: workspace_id.into(),
                focused: true,
                pane_count: 1,
                tab_count: 1,
                active_tab_id: "tab".into(),
                agent_status: crate::api::schema::AgentStatus::Unknown,
                tokens: Default::default(),
                worktree: None,
            },
        },
    })
    .unwrap()
}

fn failure(id: &str, message: &str) -> Vec<u8> {
    serde_json::to_vec(&crate::api::schema::ErrorResponse {
        id: id.into(),
        error: crate::api::schema::ErrorBody {
            code: "surface_rejected".into(),
            message: message.into(),
        },
    })
    .unwrap()
}

fn surface_set_active(message: &crate::protocol::ClientMessage) -> Option<bool> {
    let crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. } = message else {
        return None;
    };
    let request: crate::api::schema::Request = serde_json::from_str(request).ok()?;
    match request.method {
        crate::api::schema::Method::ClientShellSurfaceSet(params) => Some(params.active),
        _ => None,
    }
}

fn resize() -> crate::protocol::ClientMessage {
    crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 8,
        cell_height_px: 16,
        surface_size: crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 },
        pixel_mouse: false,
    }
}

fn surface(boot_id: &str, revision: u64, pane: &str) -> crate::protocol::PaneSurfaceFrame {
    crate::protocol::PaneSurfaceFrame {
        boot_id: boot_id.into(),
        projection_revision: revision,
        surface_revision: revision,
        frame: crate::protocol::FrameData {
            cells: Vec::new(),
            width: 80,
            height: 24,
            cursor: None,
            hyperlinks: Vec::new(),
            graphics: Vec::new(),
        },
        panes: vec![crate::protocol::PaneSurfacePane {
            pane_id: pane.into(),
            content_revision: revision,
            rect: crate::protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            inner_rect: crate::protocol::SurfaceRect {
                x: 0,
                y: 0,
                width: 80,
                height: 24,
            },
            scrollbar_rect: None,
            scroll: None,
            focused: true,
            mouse_reporting: false,
            sgr_pixel_mouse: false,
            alternate_screen_active: false,
            pixel_width: 0,
            pixel_height: 0,
        }],
        splits: Vec::new(),
        popup: None,
        graphics: Default::default(),
    }
}

fn machine() -> PendingEndpointActivation {
    PendingEndpointActivation {
        source: lease(ClientEndpointId::Local, 1, "local-boot"),
        source_available: true,
        target: lease(endpoint(), 7, "remote-boot"),
        focus: None,
        host_focused: true,
        resize: resize(),
        phase: ActivationPhase::ActivatingTarget {
            request_id: "client-shell-surface:3:on".into(),
            acknowledged_revision: Some(1),
            focus_request_id: None,
            focus_request_target: None,
            focus_acknowledged: true,
            evidence: ActivationEvidence::default(),
        },
        deadline: Instant::now() + ACTIVATION_TIMEOUT,
        epoch: 3,
        next_focus_serial: 0,
        rollback_error: None,
    }
}

#[test]
fn source_off_request_is_distinct_and_precedes_target_on_phase() {
    let activation = PendingEndpointActivation {
        source: lease(ClientEndpointId::Local, 1, "local-boot"),
        source_available: true,
        target: lease(endpoint(), 7, "remote-boot"),
        focus: None,
        host_focused: true,
        resize: resize(),
        phase: ActivationPhase::ReleasingSource {
            request_id: "client-shell-surface:9:off".into(),
        },
        deadline: Instant::now() + ACTIVATION_TIMEOUT,
        epoch: 9,
        next_focus_serial: 0,
        rollback_error: None,
    };
    assert!(activation.accepts_response(
        &ClientEndpointId::Local,
        1,
        "local-boot",
        "client-shell-surface:9:off"
    ));
    assert!(!activation.accepts_response(
        &endpoint(),
        7,
        "remote-boot",
        "client-shell-surface:9:on"
    ));
}

#[test]
fn active_source_requires_metadata_from_its_current_connection_generation() {
    let (mut shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        99,
        Box::new(test_snapshot("stale-local-boot", 1)),
    );

    let result = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        26,
        Instant::now(),
    );

    assert!(
        matches!(result, Err(ActivationBeginError::Preflight(message)) if message.contains("this connection"))
    );
    assert!(local_sent.lock().unwrap().is_empty());
    assert!(remote_sent.lock().unwrap().is_empty());
    assert!(endpoints.active_surface_available());
}

#[test]
fn source_write_failure_does_not_block_healthy_target() {
    let (shell, mut endpoints, _, remote_sent) = shell_and_registry_with_source_failure(true);
    let activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        10,
        Instant::now(),
    )
    .unwrap();
    assert!(!activation.source_available);
    assert!(matches!(
        activation.phase,
        ActivationPhase::ActivatingTarget { .. }
    ));
    assert!(remote_sent
        .lock()
        .unwrap()
        .iter()
        .any(|m| surface_set_active(m) == Some(true)));
}

#[test]
fn target_activation_starts_without_source_release_acknowledgement() {
    let (shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        11,
        Instant::now(),
    )
    .unwrap();
    let local = local_sent.lock().unwrap();
    assert_eq!(
        local.first(),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: false }),
        "source focus is revoked before source-off"
    );
    assert_eq!(
        local
            .as_slice()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false],
        "the source is released first"
    );
    drop(local);
    assert!(!remote_sent.lock().unwrap().is_empty());
    assert!(
        !endpoints.active_surface_available(),
        "pane input is blocked while frozen"
    );

    assert_eq!(
        activation.receive_response(
            &ClientEndpointId::Local,
            1,
            "client-shell-surface:11:off",
            &surface_success("client-shell-surface:11:off", false, 1),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Stale
    );
    let remote = remote_sent.lock().unwrap();
    assert!(matches!(
        remote[0],
        crate::protocol::ClientMessage::ClientShellResize { .. }
    ));
    assert_eq!(
        remote.get(2),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: true })
    );
    assert_eq!(remote.get(1).and_then(surface_set_active), Some(true));
}

#[test]
fn activation_requires_an_exact_snapshot_surface_revision_pair() {
    let mut activation = machine();
    let target = endpoint();
    let snapshot = crate::protocol::ClientShellSnapshot {
        boot_id: "remote-boot".into(),
        revision: 2,
        config_diagnostic: None,
        product_announcement: None,
        update_available: None,
        update_install_command: String::new(),
        server_keybindings_toml: None,
        latest_release_notes_available: false,
        integration_updates_available: false,
        worktree_directory: String::new(),
        release_notes: None,
        focused_workspace_id: None,
        focused_tab_id: None,
        focused_pane_id: None,
        tab_bar_right: Vec::new(),
        tab_bar_right_separator: String::new(),
        agent_view_label: None,
        agent_order: Vec::new(),
        workspaces: Vec::new(),
        tabs: Vec::new(),
        panes: Vec::new(),
        agents: Vec::new(),
        commands: Vec::new(),
    };
    assert_eq!(
        activation.receive_snapshot(&target, 7, &snapshot),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 2, "pane")),
        SurfaceActivationProgress::Ready
    );
}

#[test]
fn typed_target_ack_sets_a_floor_for_same_boot_activation_evidence() {
    let (shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        16,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:16:off",
        &surface_success("client-shell-surface:16:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        activation.receive_response(
            &target,
            7,
            "client-shell-surface:16:on",
            &surface_success("client-shell-surface:16:on", true, 4),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_snapshot(&target, 7, &test_snapshot("remote-boot", 3)),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 3, "pane")),
        SurfaceActivationProgress::Pending,
        "a delayed same-boot surface below the acknowledgement floor is not evidence"
    );
    assert_eq!(
        activation.receive_snapshot(&target, 7, &test_snapshot("remote-boot", 4)),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&target, 7, surface("remote-boot", 4, "pane")),
        SurfaceActivationProgress::Ready
    );
}

#[test]
fn stale_generation_and_boot_are_not_activation_evidence() {
    let mut activation = machine();
    assert_eq!(
        activation.receive_surface(&endpoint(), 6, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Stale
    );
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("old-boot", 1, "pane")),
        SurfaceActivationProgress::Stale
    );
}

#[test]
fn stale_response_boot_is_not_consumed() {
    let mut activation = machine();
    let (_shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    assert_eq!(
        activation.receive_response_for_boot(
            &endpoint(),
            7,
            "old-boot",
            "client-shell-surface:3:on",
            &surface_success("client-shell-surface:3:on", true, 2),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Stale
    );
}

#[test]
fn same_target_retarget_is_latest_wins() {
    let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        Some(crate::client::shell::ClientEndpointFocusTarget::Workspace(
            "old".into(),
        )),
        resize(),
        12,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:12:off",
        &surface_success("client-shell-surface:12:off", false, 1),
        &mut endpoints,
    );
    let old_focus = remote_sent
        .lock()
        .unwrap()
        .iter()
        .find_map(|message| match message {
            crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. }
                if surface_set_active(message).is_none() =>
            {
                Some(
                    serde_json::from_str::<crate::api::schema::Request>(request)
                        .unwrap()
                        .id,
                )
            }
            _ => None,
        })
        .unwrap();
    let sent_before_retarget = remote_sent.lock().unwrap().len();
    activation
        .retarget(
            Some(crate::client::shell::ClientEndpointFocusTarget::Workspace(
                "new".into(),
            )),
            &mut endpoints,
        )
        .unwrap();
    assert_eq!(remote_sent.lock().unwrap().len(), sent_before_retarget);
    assert!(activation.accepts_response(&target, 7, "remote-boot", &old_focus));
    assert_eq!(
        activation.receive_response(
            &target,
            7,
            &old_focus,
            &workspace_focus_success(&old_focus, "old"),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let latest_focus = remote_sent
        .lock()
        .unwrap()
        .last()
        .and_then(|message| match message {
            crate::protocol::ClientMessage::ClientShellEndpointRequest { request, .. } => Some(
                serde_json::from_str::<crate::api::schema::Request>(request)
                    .unwrap()
                    .id,
            ),
            _ => None,
        })
        .unwrap();
    assert_ne!(latest_focus, old_focus);
    assert!(activation.accepts_response(&target, 7, "remote-boot", &latest_focus));
}

#[test]
fn latest_host_focus_is_replayed_to_the_eventual_target() {
    let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        25,
        Instant::now(),
    )
    .unwrap();
    activation.update_host_focus(false, &mut endpoints).unwrap();

    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:25:off",
        &surface_success("client-shell-surface:25:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        remote_sent.lock().unwrap().last(),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: false })
    );

    activation.update_host_focus(true, &mut endpoints).unwrap();
    assert_eq!(
        remote_sent.lock().unwrap().last(),
        Some(&crate::protocol::ClientMessage::ClientShellFocus { focused: true })
    );
}

#[test]
fn host_focus_change_restarts_an_issued_presentation_effects_fence() {
    let (_shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let mut activation = machine();
    activation.phase = ActivationPhase::AwaitingPresentationEffects {
        lease: lease(endpoint(), 7, "remote-boot"),
        token: "old-token".into(),
        ready: false,
        completion: Box::new(ActivationCompletion::Activated),
    };

    activation.update_host_focus(false, &mut endpoints).unwrap();

    assert!(matches!(
        activation.phase,
        ActivationPhase::SynchronizingPresentation { .. }
    ));
    assert_eq!(
        activation.receive_presentation_effects_ready(&endpoint(), 7, "old-token"),
        SurfaceActivationProgress::Stale
    );
    assert_eq!(
        remote_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true]
    );
}

#[test]
fn background_release_rejection_does_not_cancel_target_activation() {
    let (shell, mut endpoints, _, remote_sent) = shell_and_registry();
    let activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        13,
        Instant::now(),
    )
    .unwrap();
    assert!(endpoints.receive_surface_release(
        &ClientEndpointId::Local,
        1,
        &crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: "local-boot".into(),
            request_id: "client-shell-surface:13:off".into(),
            final_chunk: true,
            data: failure("client-shell-surface:13:off", "source rejected release"),
        }
    ));
    assert!(endpoints.connection(&ClientEndpointId::Local).is_none());
    assert!(endpoints.connection(&endpoint()).is_some());
    assert!(matches!(
        activation.phase,
        ActivationPhase::ActivatingTarget { .. }
    ));
    assert!(remote_sent
        .lock()
        .unwrap()
        .iter()
        .any(|m| surface_set_active(m) == Some(true)));
}

#[test]
fn source_restoration_requires_its_own_acknowledgement() {
    let (shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        14,
        Instant::now(),
    )
    .unwrap();
    activation
        .start_source_restore(&mut endpoints, resize())
        .unwrap();
    let sent = local_sent.lock().unwrap();
    assert_eq!(
        sent.iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    assert!(activation.accepts_response(
        &ClientEndpointId::Local,
        1,
        "local-boot",
        "client-shell-surface:14:rollback-source-on"
    ));
}

#[test]
fn resize_invalidates_already_recorded_surface_evidence() {
    let (_shell, mut endpoints, _local_sent, _remote_sent) = shell_and_registry();
    let mut activation = machine();
    assert_eq!(
        activation.receive_snapshot(&endpoint(), 7, &test_snapshot("remote-boot", 1)),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Ready
    );
    let resize = crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 9,
        cell_height_px: 17,
        surface_size: crate::protocol::ClientSurfaceSize {
            cols: 100,
            rows: 30,
        },
        pixel_mouse: true,
    };
    activation.update_resize(resize, &mut endpoints).unwrap();
    assert_eq!(activation.progress(), SurfaceActivationProgress::Pending);
}

#[test]
fn resize_during_activation_reaches_the_pending_target() {
    let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        15,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:15:off",
        &surface_success("client-shell-surface:15:off", false, 1),
        &mut endpoints,
    );
    let resized = crate::protocol::ClientMessage::ClientShellResize {
        cell_width_px: 9,
        cell_height_px: 17,
        surface_size: crate::protocol::ClientSurfaceSize {
            cols: 100,
            rows: 30,
        },
        pixel_mouse: true,
    };
    activation
        .update_resize(resized.clone(), &mut endpoints)
        .unwrap();
    assert_eq!(remote_sent.lock().unwrap().last(), Some(&resized));
    assert_eq!(
        activation.receive_surface(&endpoint(), 7, surface("remote-boot", 1, "pane")),
        SurfaceActivationProgress::Pending,
        "a surface for the prior geometry cannot commit"
    );
}

#[test]
fn rapid_a_to_b_to_a_starts_local_without_waiting_for_remote_release() {
    let (shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    let mut old = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        20,
        Instant::now(),
    )
    .unwrap();
    let intent = old.supersede(ClientEndpointId::Local, None, &mut endpoints);
    let mut next = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        intent.endpoint_id,
        intent.target,
        resize(),
        21,
        Instant::now(),
    )
    .unwrap();
    assert!(matches!(
        next.phase,
        ActivationPhase::ActivatingTarget { .. }
    ));
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
    assert_eq!(
        remote_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true, false]
    );
    assert_eq!(
        next.receive_surface(&endpoint(), 7, surface("remote-boot", 2, "pane")),
        SurfaceActivationProgress::Stale
    );
    assert!(!endpoints.active_surface_available());
}

#[test]
fn disconnected_committed_source_does_not_block_switching_to_a_live_endpoint() {
    let (mut shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let disconnected = endpoint();
    endpoints.set_surface_active(&ClientEndpointId::Local, false);
    endpoints.set_surface_active(&disconnected, true);
    assert!(endpoints.set_active(&disconnected));
    assert!(shell.activate_endpoint_projection(&disconnected));
    endpoints.disconnect(&disconnected);

    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        ClientEndpointId::Local,
        None,
        resize(),
        22,
        Instant::now(),
    )
    .unwrap();
    assert_eq!(activation.source_command_lane(), None);
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true],
        "there is no unavailable source release to await"
    );

    assert_eq!(
        activation.receive_response(
            &ClientEndpointId::Local,
            1,
            "client-shell-surface:22:on",
            &surface_success("client-shell-surface:22:on", true, 2),
            &mut endpoints,
        ),
        SurfaceActivationProgress::Pending
    );
    let snapshot = test_snapshot("local-boot", 2);
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(snapshot.clone()),
    );
    assert_eq!(
        activation.receive_snapshot(&ClientEndpointId::Local, 1, &snapshot),
        SurfaceActivationProgress::Pending
    );
    assert_eq!(
        activation.receive_surface(
            &ClientEndpointId::Local,
            1,
            surface("local-boot", 2, "pane")
        ),
        SurfaceActivationProgress::Ready
    );
    assert!(matches!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::AwaitingPresentationSync {
            previous,
            endpoint: ClientEndpointId::Local,
        }) if previous == disconnected
    ));
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:22:presentation-sync",
        &surface_success("client-shell-surface:22:presentation-sync", true, 3),
        &mut endpoints,
    );
    let sync_snapshot = test_snapshot("local-boot", 3);
    shell.set_endpoint_snapshot_for_generation(
        &ClientEndpointId::Local,
        1,
        Box::new(sync_snapshot.clone()),
    );
    let _ = activation.receive_snapshot(&ClientEndpointId::Local, 1, &sync_snapshot);
    assert_eq!(
        activation.receive_surface(
            &ClientEndpointId::Local,
            1,
            surface("local-boot", 3, "pane")
        ),
        SurfaceActivationProgress::Ready
    );
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::AwaitingPresentationEffects)
    );
    assert_eq!(
        activation.receive_presentation_effects_ready(
            &ClientEndpointId::Local,
            1,
            "22:1:local-boot"
        ),
        SurfaceActivationProgress::Ready
    );
    assert_eq!(
        activation.complete(&mut shell, &mut endpoints),
        Ok(ActivationCompletion::Activated)
    );
    assert_eq!(endpoints.active_id(), &ClientEndpointId::Local);
    assert_ne!(endpoints.active_id(), &disconnected);
}

#[test]
fn new_selection_does_not_wait_for_in_flight_source_restoration() {
    let (shell, mut endpoints, _, remote_sent) = shell_and_registry();
    let mut old = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        23,
        Instant::now(),
    )
    .unwrap();
    old.start_source_restore(&mut endpoints, resize()).unwrap();
    let intent = old.supersede(endpoint(), None, &mut endpoints);
    let next = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        intent.endpoint_id,
        intent.target,
        resize(),
        24,
        Instant::now(),
    )
    .unwrap();
    assert!(matches!(
        next.phase,
        ActivationPhase::ActivatingTarget { .. }
    ));
    assert_eq!(
        remote_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![true, true]
    );
}

#[test]
fn unacknowledged_target_release_closes_target_before_restoring_source() {
    let (shell, mut endpoints, local_sent, _remote_sent) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        24,
        Instant::now(),
    )
    .unwrap();
    let _ = activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:24:off",
        &surface_success("client-shell-surface:24:off", false, 1),
        &mut endpoints,
    );
    assert_eq!(
        activation.rollback(&mut endpoints, "target activation timed out".into(), false),
        ActivationRollback::Pending
    );
    assert_eq!(
        activation.rollback(&mut endpoints, "target release timed out".into(), false),
        ActivationRollback::Pending
    );
    assert!(endpoints.connection(&target).is_none());
    let failures = endpoints.take_failures();
    assert_eq!(
        failures.len(),
        1,
        "rollback revocation must reach the reconnect owner"
    );
    assert_eq!(failures[0].endpoint_id, target);
    assert_eq!(failures[0].generation, 7);
    assert_eq!(failures[0].kind, std::io::ErrorKind::TimedOut);
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn target_loss_at_activation_deadline_restores_source_before_timeout() {
    let (shell, mut endpoints, local_sent, _) = shell_and_registry();
    let target = endpoint();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        target.clone(),
        None,
        resize(),
        30,
        Instant::now(),
    )
    .unwrap();
    activation.receive_response(
        &ClientEndpointId::Local,
        1,
        "client-shell-surface:30:off",
        &surface_success("client-shell-surface:30:off", false, 1),
        &mut endpoints,
    );
    let now = Instant::now();
    activation.deadline = now;
    assert!(activation.expired(now));
    endpoints.fail(&target, std::io::ErrorKind::UnexpectedEof.into());
    // Match the client timer: apply transport failures before checking phase expiry.
    for failure in endpoints.take_failures() {
        assert_eq!(
            activation.endpoint_disconnected(&mut endpoints, &failure.endpoint_id, failure.message),
            ActivationRollback::Pending
        );
    }
    assert!(matches!(
        activation.phase,
        ActivationPhase::RestoringSource { .. }
    ));
    assert!(!activation.expired(now));
    assert_eq!(
        local_sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(surface_set_active)
            .collect::<Vec<_>>(),
        vec![false, true]
    );
}

#[test]
fn losing_local_during_handoff_does_not_revoke_the_healthy_target() {
    for source_released in [false, true] {
        let (shell, mut endpoints, _local_sent, remote_sent) = shell_and_registry();
        let target = endpoint();
        let mut activation = PendingEndpointActivation::begin(
            &shell,
            &mut endpoints,
            target.clone(),
            None,
            resize(),
            29,
            Instant::now(),
        )
        .unwrap();
        if source_released {
            assert!(endpoints.receive_surface_release(
                &ClientEndpointId::Local,
                1,
                &crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
                    boot_id: "local-boot".into(),
                    request_id: "client-shell-surface:29:off".into(),
                    final_chunk: true,
                    data: surface_success("client-shell-surface:29:off", false, 1),
                }
            ));
        }
        endpoints.fail(
            &ClientEndpointId::Local,
            std::io::ErrorKind::BrokenPipe.into(),
        );
        assert_eq!(
            activation.endpoint_disconnected(
                &mut endpoints,
                &ClientEndpointId::Local,
                "Local stopped".into()
            ),
            ActivationRollback::Pending
        );
        assert!(!activation.source_available);
        assert!(
            !activation.expired(Instant::now()),
            "starting the healthy target must get a fresh deadline"
        );
        assert!(matches!(
            activation.phase,
            ActivationPhase::ActivatingTarget { .. }
        ));
        assert!(endpoints.connection(&target).is_some());
        assert_eq!(
            remote_sent
                .lock()
                .unwrap()
                .iter()
                .filter_map(surface_set_active)
                .collect::<Vec<_>>(),
            vec![true]
        );
    }
}

#[test]
fn resize_message_preserves_the_latest_surface_dimensions() {
    assert_eq!(
        resize_geometry(&resize()),
        Some(crate::protocol::ClientSurfaceSize { cols: 80, rows: 24 })
    );
}

#[test]
fn failed_source_restoration_reconnects_instead_of_leaving_input_frozen() {
    // Exercise the initial restore, its coherent-frame fence, and its host-effects fence.
    for phase in 0..3 {
        let (mut shell, mut endpoints, _, _) = shell_and_registry();
        let mut activation = PendingEndpointActivation::begin(
            &shell,
            &mut endpoints,
            endpoint(),
            None,
            resize(),
            90,
            Instant::now(),
        )
        .unwrap();
        activation
            .start_source_restore(&mut endpoints, resize())
            .unwrap();
        if phase > 0 {
            let request_id = "client-shell-surface:90:rollback-source-on";
            activation.receive_response(
                &ClientEndpointId::Local,
                1,
                request_id,
                &surface_success(request_id, true, 2),
                &mut endpoints,
            );
            let snapshot = test_snapshot("local-boot", 2);
            shell.set_snapshot(Box::new(snapshot.clone()));
            activation.receive_snapshot(&ClientEndpointId::Local, 1, &snapshot);
            assert_eq!(
                activation.receive_surface(
                    &ClientEndpointId::Local,
                    1,
                    surface("local-boot", 2, "pane")
                ),
                SurfaceActivationProgress::Ready
            );
            assert!(matches!(
                activation.complete(&mut shell, &mut endpoints),
                Ok(ActivationCompletion::AwaitingPresentationSync { .. })
            ));
        }
        if phase > 1 {
            let request_id = "client-shell-surface:90:presentation-sync";
            activation.receive_response(
                &ClientEndpointId::Local,
                1,
                request_id,
                &surface_success(request_id, true, 3),
                &mut endpoints,
            );
            let snapshot = test_snapshot("local-boot", 3);
            shell.set_endpoint_snapshot_for_generation(
                &ClientEndpointId::Local,
                1,
                Box::new(snapshot.clone()),
            );
            activation.receive_snapshot(&ClientEndpointId::Local, 1, &snapshot);
            assert_eq!(
                activation.receive_surface(
                    &ClientEndpointId::Local,
                    1,
                    surface("local-boot", 3, "pane")
                ),
                SurfaceActivationProgress::Ready
            );
            assert_eq!(
                activation.complete(&mut shell, &mut endpoints),
                Ok(ActivationCompletion::AwaitingPresentationEffects)
            );
        }
        assert!(!endpoints.active_surface_available());
        assert!(matches!(
            activation.rollback(&mut endpoints, "restore timed out".into(), false,),
            ActivationRollback::Unavailable(_)
        ));
        assert!(
            endpoints.connection(&ClientEndpointId::Local).is_none(),
            "phase {phase}: the stalled source must be retired so the supervisor reconnects it"
        );
        assert!(
            endpoints.connection(&endpoint()).is_some(),
            "an unrelated remote connection must survive"
        );
        assert!(
            !endpoints.active_surface_available(),
            "input must remain gated until a new coherent activation completes"
        );
        let failures = endpoints.take_failures();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].endpoint_id, ClientEndpointId::Local);
        assert_eq!(failures[0].generation, 1);
        assert!(failures[0].message.contains("restore timed out"));
    }
}

#[test]
fn failed_old_restoration_does_not_disconnect_a_new_source_generation() {
    let (shell, mut endpoints, local_sent, _) = shell_and_registry();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        91,
        Instant::now(),
    )
    .unwrap();
    activation
        .start_source_restore(&mut endpoints, resize())
        .unwrap();
    endpoints.insert(
        ClientEndpointId::Local,
        FakeTransport {
            sent: local_sent,
            fail_after_write: false,
        },
        2,
        negotiation(),
        false,
    );
    assert!(matches!(
        activation.rollback(&mut endpoints, "old restore timed out".into(), false),
        ActivationRollback::Unavailable(_)
    ));
    assert!(endpoints.accepts(&ClientEndpointId::Local, 2));
    assert!(endpoints.take_failures().is_empty());
}

#[test]
fn silent_remote_release_expires_independently_of_local_activation() {
    let (shell, mut endpoints, local_sent, remote_sent) = shell_and_registry();
    let remote = endpoint();
    endpoints.set_surface_active(&remote, true);
    assert!(endpoints.set_active(&remote));
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        ClientEndpointId::Local,
        None,
        resize(),
        991,
        Instant::now(),
    )
    .unwrap();
    assert!(!endpoints.active_surface_available());
    assert!(local_sent
        .lock()
        .unwrap()
        .iter()
        .any(|m| surface_set_active(m) == Some(true)));
    endpoints.tick_health(Instant::now() + ACTIVATION_TIMEOUT);
    let failures = endpoints.take_failures();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].endpoint_id, remote);
    assert_eq!(
        activation.endpoint_disconnected(&mut endpoints, &remote, "timeout".into()),
        ActivationRollback::Pending
    );
    assert!(matches!(
        activation.phase,
        ActivationPhase::ActivatingTarget { .. }
    ));
    assert!(local_sent
        .lock()
        .unwrap()
        .iter()
        .any(|m| surface_set_active(m) == Some(true)));
    assert!(
        !remote_sent
            .lock()
            .unwrap()
            .iter()
            .any(|m| surface_set_active(m) == Some(true)),
        "never restore the timed-out source"
    );
    assert!(
        !endpoints.active_surface_available(),
        "input still waits for coherent Local evidence"
    );
}

#[test]
fn repeated_selection_returns_latest_intent_without_remote_round_trip() {
    let (shell, mut endpoints, _, _) = shell_and_registry();
    let mut old = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        992,
        Instant::now(),
    )
    .unwrap();
    let first = old.supersede(endpoint(), None, &mut endpoints);
    assert_eq!(first.endpoint_id, endpoint());
    let latest = old.supersede(ClientEndpointId::Local, None, &mut endpoints);
    assert_eq!(latest.endpoint_id, ClientEndpointId::Local);
}

#[test]
fn background_release_timeout_does_not_retire_a_new_connection_generation() {
    let (shell, mut endpoints, sent, _) = shell_and_registry();
    let _activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        993,
        Instant::now(),
    )
    .unwrap();
    endpoints.insert(
        ClientEndpointId::Local,
        FakeTransport {
            sent,
            fail_after_write: false,
        },
        2,
        negotiation(),
        false,
    );
    endpoints.tick_health(Instant::now() + ACTIVATION_TIMEOUT);
    assert!(endpoints.accepts(&ClientEndpointId::Local, 2));
    assert!(endpoints.take_failures().is_empty());
}

#[test]
fn cancelled_background_release_cannot_disconnect_reactivated_endpoint() {
    let (shell, mut endpoints, _, _) = shell_and_registry();
    let now = Instant::now();
    let mut activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        994,
        now,
    )
    .unwrap();
    activation
        .start_source_restore(&mut endpoints, resize())
        .unwrap();
    assert!(!endpoints.receive_surface_release(
        &ClientEndpointId::Local,
        1,
        &crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
            boot_id: "local-boot".into(),
            request_id: "client-shell-surface:994:off".into(),
            final_chunk: true,
            data: failure("client-shell-surface:994:off", "late rejection"),
        }
    ));
    endpoints.tick_health(now + ACTIVATION_TIMEOUT);
    assert!(endpoints.accepts(&ClientEndpointId::Local, 1));
    assert!(endpoints.take_failures().is_empty());
}

#[test]
fn background_release_ack_is_correlated_and_prevents_cleanup_timeout() {
    let (shell, mut endpoints, _, _) = shell_and_registry();
    let now = Instant::now();
    let _activation = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        995,
        now,
    )
    .unwrap();
    let message = crate::protocol::ServerMessage::ClientShellEndpointResponseChunk {
        boot_id: "local-boot".into(),
        request_id: "client-shell-surface:995:off".into(),
        final_chunk: true,
        data: surface_success("client-shell-surface:995:off", false, 2),
    };
    assert!(!endpoints.receive_surface_release(&ClientEndpointId::Local, 2, &message));
    assert!(endpoints.receive_surface_release(&ClientEndpointId::Local, 1, &message));
    endpoints.tick_health(now + ACTIVATION_TIMEOUT);
    assert!(endpoints.accepts(&ClientEndpointId::Local, 1));
    assert!(endpoints.take_failures().is_empty());
}

#[test]
fn clicking_partially_projected_target_cannot_take_already_active_shortcut() {
    let (shell, mut endpoints, _, _) = shell_and_registry();
    let mut old = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        endpoint(),
        None,
        resize(),
        996,
        Instant::now(),
    )
    .unwrap();
    endpoints.set_surface_active(&endpoint(), true);
    endpoints.set_active(&endpoint());
    let intent = old.supersede(endpoint(), None, &mut endpoints);
    assert!(!endpoints.connection(&endpoint()).unwrap().surface_active);
    let next = PendingEndpointActivation::begin(
        &shell,
        &mut endpoints,
        intent.endpoint_id,
        intent.target,
        resize(),
        997,
        Instant::now(),
    )
    .unwrap();
    assert!(matches!(
        next.phase,
        ActivationPhase::ActivatingTarget { .. }
    ));
}
