#!/usr/bin/env python3
"""Verify cached switches and local submission feedback on a dedicated SSH session.

Requires a running test server and a ready Claude test agent. Uses a temporary
local catalog, seeds harmless display prompts, and preserves user sessions.
"""
import sys, os, json, tempfile, subprocess, time, re, shutil
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from local_chat_ssh_e2e import UI, BIN, SSH, SESSION, AGENT, api, OUT

root = Path(tempfile.mkdtemp(prefix="hp-", dir="/tmp"))
(root / "config.toml").write_text('onboarding = false\n[keys]\nprefix = "ctrl+a"\n')
env = dict(
    os.environ,
    HERDR_CONFIG_PATH=str(root / "config.toml"),
    XDG_CONFIG_HOME=str(root / "cfg"),
    XDG_STATE_HOME=str(root / "state"),
    HERDR_LOG="herdr=info,herdr::client::diagnostics=trace",
)
env.pop("HERDR_ENV", None)
for app in ["herdr", "herdr-dev"]:
    catalog = root / f"state/{app}/client"
    catalog.mkdir(parents=True)
    profile = {
        "id": "0123456789abcdef0123456789abcdef",
        "label": "preview-target",
        "target": SSH,
        "session": SESSION,
        "enabled": True,
    }
    (catalog / "endpoints.json").write_text(
        json.dumps({"version": 1, "selected_profile": None, "ssh": [profile]})
    )
    (catalog / "endpoint-selection.json").write_text(
        json.dumps({"version": 1, "selected_profile": None})
    )
api(
    "agent",
    "prompt",
    AGENT,
    "Reply exactly BASE_SESSION_READY. Do not use tools or change files.",
    "--wait",
    "--timeout",
    "60000",
)
ui = UI(launch=[BIN, "--session", "preview-client-e2e-1028"], env_overrides=env)
checks = []
timings = []
switches = []


def click(label):
    row = next(j for j, l in enumerate(ui.screen.display) if label in l)
    os.write(ui.master, f"\x1b[<0;9;{row+1}M\x1b[<0;9;{row+1}m".encode())


def wait_until(fn, timeout=40):
    deadline = time.monotonic() + timeout
    while not fn():
        ui.pump(0.005)
        if time.monotonic() > deadline:
            ui.snap("FAIL-timeout")
            raise AssertionError("condition timed out")


try:
    ui.wait("preview-target", 60)
    ui.pump(2)
    client_log = next(
        (root / "cfg").glob("*/sessions/preview-client-e2e-1028/herdr-client.log")
    )

    def done():
        return client_log.read_text().count("machine activation completed endpoint=Ssh")

    click("preview-target")
    ui.wait("input paused", 3)
    checks.append(
        "first visit selects the target immediately and labels missing cache without granting input"
    )
    wait_until(lambda: done() >= 1)
    ui.wait("Sonnet")
    ui.send("\x1b[17~")
    ui.wait("Conversation", 60)
    ui.wait("BASE_SESSION_READY", 60)
    ui.snap("01-loaded-reader")
    local_before = client_log.read_text().count(
        "machine activation completed endpoint=Local"
    )
    click("Local")
    wait_until(
        lambda: client_log.read_text().count(
            "machine activation completed endpoint=Local"
        )
        > local_before
    )
    remote_before = done()
    click("preview-target")
    wait_until(
        lambda: "Cached view" in ui.text() and "BASE_SESSION_READY" in ui.text(), 3
    )
    local_before = client_log.read_text().count(
        "machine activation completed endpoint=Local"
    )
    click("Local")
    wait_until(
        lambda: client_log.read_text().count(
            "machine activation completed endpoint=Local"
        )
        > local_before
    )
    click("preview-target")
    wait_until(lambda: done() > remote_before)
    checks.append(
        "rapid cached remote/local/remote retargeting completes without granting the old source an input lease"
    )
    for i, (cols, rows) in enumerate(
        [(160, 48), (106, 24), (197, 59), (80, 24), (240, 80), (160, 48)]
    ):
        ui.resize(cols, rows)
        click("Local")
        wait_until(
            lambda: "Local · Cached view" in ui.text()
            or "Local · Connecting" in ui.text()
            or "BASE_SESSION_READY" not in ui.text()
        )
        ui.pump(0.25)
        completed = done()
        start = time.monotonic()
        click("preview-target")
        wait_until(
            lambda: "Cached view" in ui.text() and "BASE_SESSION_READY" in ui.text(), 3
        )
        elapsed = round((time.monotonic() - start) * 1000, 2)
        assert "BASE_SESSION_READY" in ui.text(), ui.text()
        assert elapsed < 250, (elapsed, ui.text())
        os.write(ui.master, b"SHOULD_NOT_ENTER_EITHER_DRAFT")
        ui.pump(0.05)
        assert "SHOULD_NOT_ENTER_EITHER_DRAFT" not in ui.text()
        ui.snap(f"02-cached-switch-{i}")
        wait_until(lambda: done() > completed)
        assert "SHOULD_NOT_ENTER_EITHER_DRAFT" not in ui.text()
        switches.append({"geometry": f"{cols}x{rows}", "cached_preview_ms": elapsed})
        print(json.dumps(switches[-1]), flush=True)
    checks.append(
        "six cached switches paint immediately across four sizes; typing remains blocked during the preview"
    )
    ui.resize(160, 48)
    runid = f"{time.time_ns():x}"
    for i in range(3):
        reply = f"REPLY_{runid}_{i}"
        prompt = f"Reply exactly {reply}. Do not use tools or change files."
        ui.send("\x1b[200~" + prompt + "\x1b[201~")
        previous = client_log.read_text().count("local chat submission acknowledged")
        start = time.monotonic()
        os.write(ui.master, b"\r")
        wait_until(lambda: "YOU · sending" in ui.text() and prompt in ui.text(), 3)
        local_ms = round((time.monotonic() - start) * 1000, 2)
        assert local_ms < 250, local_ms
        assert prompt in ui.text(), ui.text()
        ui.snap(f"03-local-send-{i}")
        wait_until(
            lambda: client_log.read_text().count("local chat submission acknowledged")
            > previous,
            30,
        )
        ack_ms = int(
            re.findall(
                r"local chat submission acknowledged.*elapsed_ms=(\d+)",
                client_log.read_text(),
            )[-1]
        )
        wait_until(
            lambda: any(
                line.split("│", 1)[-1].strip(" │┃") == reply
                for line in ui.screen.display
            ),
            60,
        )
        reply_ms = round((time.monotonic() - start) * 1000, 2)
        wait_until(
            lambda: "YOU · sending" not in ui.text() and "YOU · sent" not in ui.text(),
            30,
        )
        assert ui.text().count(prompt) == 1, ui.text()
        timings.append(
            {
                "local_echo_ms": local_ms,
                "remote_submission_ack_ms": ack_ms,
                "assistant_reply_visible_ms_including_model": reply_ms,
            }
        )
        print(json.dumps(timings[-1]), flush=True)
    checks.append(
        "three real Claude submissions echo locally before acknowledgement, then reconcile once with saved history"
    )
    ui.snap("04-final-replies")
    ui.pump(15)
    log = client_log.read_text()
    assert "endpoint transport failed" not in log and "retiring unresponsive" not in log
    checks.append(
        "no transport failures through cached switching, live sends and 15 seconds idle"
    )
    (OUT / "results.json").write_text(
        json.dumps({"checks": checks, "switches": switches, "sends": timings}, indent=2)
    )
    print(
        json.dumps(
            {"checks": checks, "switches": switches, "sends": timings}, indent=2
        ),
        flush=True,
    )
finally:
    ui.close()
    subprocess.run(
        [BIN, "--session", "preview-client-e2e-1028", "server", "stop"],
        env=env,
        stdout=subprocess.DEVNULL,
    )
    shutil.copytree(root, OUT / "fixture", dirs_exist_ok=True)
    print("fixture", root, flush=True)
