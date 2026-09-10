use std::io;
use std::path::PathBuf;

use super::attach::{find_installed_remote_herdr, RemoteSsh, SshStdioBridge};

pub(crate) struct SavedSshBridge {
    _bridge: SshStdioBridge,
    // Keep the private SSH master and config alive until its bridge stops.
    _ssh: RemoteSsh,
}

pub(crate) struct SavedSshStream {
    pub(crate) stream: crate::ipc::LocalStream,
    pub(crate) bridge: SavedSshBridge,
}

pub(crate) fn connect_saved_ssh(
    profile_id: &str,
    target: &str,
    session: &str,
) -> io::Result<SavedSshStream> {
    validate_profile_path_id(profile_id)?;
    crate::session::validate_name(session)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    let ssh = RemoteSsh::new_noninteractive(
        target.to_owned(),
        crate::config::Config::load()
            .config
            .remote
            .manage_ssh_config,
    );
    let remote_herdr = find_installed_remote_herdr(&ssh)?;
    let path = saved_bridge_path(profile_id);
    let bridge = SshStdioBridge::start(
        target.to_owned(),
        remote_herdr,
        path.clone(),
        session.to_owned(),
        ssh.options(),
        true,
    )?;
    let stream = crate::ipc::connect_local_stream(&path)?;
    Ok(SavedSshStream {
        stream,
        bridge: SavedSshBridge {
            _bridge: bridge,
            _ssh: ssh,
        },
    })
}

pub(crate) fn saved_ssh_bootstrap_command(target: &str, session: &str) -> String {
    format!(
        "herdr --remote {} --session {}",
        super::shell_quote(target),
        super::shell_quote(session)
    )
}

pub(crate) fn saved_ssh_failure_needs_attention(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::InvalidInput
            | io::ErrorKind::InvalidData
            | io::ErrorKind::NotFound
            | io::ErrorKind::PermissionDenied
            | io::ErrorKind::Unsupported
    ) {
        return true;
    }
    let message = error.to_string().to_ascii_lowercase();
    [
        "permission denied",
        "authentication failed",
        "unauthorized",
        "invalid token",
        "host key verification failed",
        "remote host identification has changed",
        "no matching host key",
        "unsupported remote platform",
        "not ready",
        "install or update",
        "protocol",
        // SSH proxies also report transient HTTP failures as a "bad handshake".
        // Only an explicit rejection requires setup; transport failures retry.
        "handshake rejected",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

fn saved_bridge_path(profile_id: &str) -> PathBuf {
    let pid = std::process::id();
    let readable = format!("herdr-ssh-{pid}-{profile_id}.sock");
    let short = format!("herdr-s-{pid}-{}.sock", &profile_id[..16]);
    crate::platform::remote_bridge_endpoint_path(&readable, &short)
}

fn validate_profile_path_id(profile_id: &str) -> io::Result<()> {
    if profile_id.len() == 32
        && profile_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid SSH endpoint profile id",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bridge_paths_use_profile_identity_not_target_or_session() {
        let first = saved_bridge_path("0123456789abcdef0123456789abcdef");
        let second = saved_bridge_path("fedcba9876543210fedcba9876543210");
        assert_ne!(first, second);
        assert!(!first.to_string_lossy().contains("example.com"));
        assert!(!first.to_string_lossy().contains("default"));
    }

    #[test]
    fn bootstrap_command_preserves_the_explicit_remote_session() {
        assert_eq!(
            saved_ssh_bootstrap_command("build host", "agent work"),
            "herdr --remote 'build host' --session 'agent work'"
        );
    }

    #[test]
    fn transient_proxy_handshake_failures_retry() {
        for message in [
            "remote platform detection failed: connect: websocket: bad handshake (no healthy upstream)",
            "remote platform detection failed: connect: websocket: bad handshake (503 Service Unavailable)",
            "SSH handshake timed out",
        ] {
            assert!(
                !saved_ssh_failure_needs_attention(&io::Error::other(message)),
                "transient failure should retry: {message}",
            );
        }
    }

    #[test]
    fn proxy_authentication_failures_require_attention() {
        for message in [
            "connect: websocket: bad handshake (invalid token)",
            "SSH proxy: unauthorized",
            "SSH proxy: authentication failed",
        ] {
            assert!(
                saved_ssh_failure_needs_attention(&io::Error::other(message)),
                "authentication failure needs attention: {message}",
            );
        }
    }

    #[test]
    fn prompt_and_compatibility_failures_require_attention() {
        for message in [
            "Permission denied (publickey)",
            "Host key verification failed",
            "matching Herdr is not ready; install or update",
            "handshake rejected",
        ] {
            assert!(saved_ssh_failure_needs_attention(&io::Error::other(
                message
            )));
        }
        assert!(!saved_ssh_failure_needs_attention(&io::Error::new(
            io::ErrorKind::TimedOut,
            "network timed out"
        )));
    }
    #[test]
    #[ignore = "requires an explicitly provided SSH target and disposable remote session"]
    fn saved_ssh_probe_on_disposable_remote_session() {
        let target = std::env::var("HERDR_TEST_SSH_TARGET").expect("explicit test target");
        let session = std::env::var("HERDR_TEST_SSH_SESSION").expect("disposable test session");
        assert!(session.starts_with("herdr-test-"));
        let started = std::time::Instant::now();
        let mut connection =
            connect_saved_ssh("fedcba9876543210fedcba9876543210", &target, &session).unwrap();
        let negotiation =
            crate::client::probe_endpoint_negotiation(&mut connection.stream).unwrap();
        assert!(negotiation.supports_surface_interest());
        eprintln!("saved SSH ready in {:.3}s", started.elapsed().as_secs_f64());
        drop(connection);
    }
}
