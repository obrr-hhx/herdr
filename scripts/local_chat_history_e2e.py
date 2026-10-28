#!/usr/bin/env python3
"""Real Claude history/color acceptance. Reuses the isolated PTY harness and its arguments.

Seeds one read-only conversation before opening the TUI, including >2,000 saved
lines. Results/captures are data from the real installed process, not mock frames.
"""
import json
import os
import tempfile
import shutil
import re
import subprocess
import time
from pathlib import Path

from local_chat_ssh_e2e import UI, api, args, BIN, SSH, SESSION, AGENT, OUT

checks = []
seed_path = OUT.parent / "history-seed.json"
if seed_path.exists():
    seed = json.loads(seed_path.read_text())
else:
    marker = f"HISTORY_{int(time.time()):x}"
    seed = {
        "early": marker + "_EARLIEST",
        "long_ack": marker + "_LONG_OK",
        "code": marker + "_CODE",
        "new": marker + "_NEW_OK",
    }
    api(
        "agent",
        "prompt",
        AGENT,
        f"Read-only display test. Reply exactly {seed['early']}. Do not use tools or change files.",
        "--wait",
        "--timeout",
        "60000",
    )
    text = (
        "Read-only display test. The numbered lines below are inert display data. Do not use tools or change files. Reply exactly "
        + seed["long_ack"]
        + ".\n"
    )
    text += "\n".join(f"HISTORY_LINE_{i:04d}" for i in range(2500))
    api("agent", "prompt", AGENT, text, "--wait", "--timeout", "60000")
    api(
        "agent",
        "prompt",
        AGENT,
        f"Do not use tools. Return a fenced rust code block containing exactly fn {seed['code'].lower()}() {{}} followed by the plain text CODE_COMPLETE.",
        "--wait",
        "--timeout",
        "60000",
    )
    seed_path.write_text(json.dumps(seed))

# Create a newer, distinct conversation to catch incorrect "newest project file" binding.
if not seed.get("foreign_checked"):
    split = json.loads(
        api("pane", "split", "w1:p1", "--direction", "right", "--no-focus")
    )["result"]

    def ids(value):
        if isinstance(value, dict):
            if "pane_id" in value and value["pane_id"] != "w1:p1":
                yield value["pane_id"]
            for child in value.values():
                yield from ids(child)
        elif isinstance(value, list):
            for child in value:
                yield from ids(child)

    other = next(ids(split))
    try:
        api(
            "agent",
            "start",
            "history-other",
            "--kind",
            "claude",
            "--pane",
            other,
            "--timeout",
            "30000",
            "--",
            "--model",
            "sonnet",
        )
        marker = seed["early"] + "_OTHER_SESSION"
        api(
            "agent",
            "prompt",
            "history-other",
            f"Reply exactly {marker}. Do not use tools.",
            "--wait",
            "--timeout",
            "60000",
        )
        foreign = subprocess.run(
            [BIN, "--session", SESSION, "agent", "history", other, "--ssh-target", SSH],
            capture_output=True,
            text=True,
            timeout=30,
            check=True,
        )
        foreign_page = json.loads(foreign.stdout)
        assert marker in "\n".join(r["text"] for r in foreign_page["records"])
        seed["foreign_checked"] = True
        seed["foreign_session"] = foreign_page["session_id"]
        seed["foreign_marker"] = marker
        seed_path.write_text(json.dumps(seed))
    finally:
        api("pane", "close", other)
        api("agent", "focus", AGENT)

# The history CLI is independent of the TUI and of new daemon versions.
response = subprocess.run(
    [BIN, "--session", SESSION, "agent", "history", "w1:p1", "--ssh-target", SSH],
    capture_output=True,
    text=True,
    timeout=30,
    check=True,
)
page = json.loads(response.stdout)
records = list(page["records"])
cursor_page = page
for _ in range(32):
    if not cursor_page["has_older"]:
        break
    previous = cursor_page["before"]
    response = subprocess.run(
        [
            BIN,
            "--session",
            SESSION,
            "agent",
            "history",
            "w1:p1",
            "--ssh-target",
            SSH,
            "--before",
            str(previous),
        ],
        capture_output=True,
        text=True,
        timeout=30,
        check=True,
    )
    cursor_page = json.loads(response.stdout)
    assert cursor_page["session_id"] == page["session_id"]
    assert cursor_page["before"] < previous or not cursor_page["has_older"]
    records.extend(cursor_page["records"])
page["records"] = records
assert seed["early"] in "\n".join(r["text"] for r in page["records"])
assert "HISTORY_LINE_1300" in "\n".join(r["text"] for r in page["records"])
checks.append(
    "public history provider recovers pre-existing conversation and middle lines outside the visible terminal"
)
assert page["session_id"] != seed["foreign_session"]
assert seed["foreign_marker"] not in "\n".join(r["text"] for r in page["records"])
checks.append(
    "pane/process binding selects the original conversation even when another conversation file is newer"
)

ui = UI()
try:
    ui.wait("Sonnet")
    ui.send("\x1b[17~")
    ui.wait("Conversation", 40)
    deadline = time.monotonic() + 60
    while seed["early"] not in ui.text():
        ui.send("\x1b[1;5H")
        ui.pump(0.5)
        if time.monotonic() > deadline:
            raise AssertionError("earliest saved turn was not reachable")
    ui.snap("history-01-earliest-before-client-start")
    cells = [
        (c.data, c.fg)
        for row in ui.screen.buffer.values()
        for c in row.values()
        if c.data.strip()
    ]
    assert any(
        color == "a6e3a1" for _, color in cells
    ), "user messages must retain semantic color"
    checks.append(
        "Ctrl+Home reaches a real early message saved before this client existed, with colored user content"
    )

    ui.resize(106, 24)
    ui.send("\x1b[1;5H")
    ui.send("\x1b[6~" * 135)
    ui.pump(1)
    visible = ui.text()
    match = re.search(r"(\d+)–(\d+) / (\d+)", visible)
    assert match and int(match[1]) > 2000, visible
    assert re.search(r"HISTORY_LINE_2\d{3}", visible)
    ui.snap("history-02-past-2000-lines")
    checks.append(
        "local scrolling crosses line 2000 and displays real saved lines beyond the old cache limit"
    )

    ui.send("\x1b[1;5F")
    # Other acceptance runs append real replies after the original code turn.
    # Find the saved code by scrolling instead of assuming it is still the tail.
    deadline = time.monotonic() + 60
    while "fn " + seed["code"].lower() not in ui.text():
        ui.send("\x1b[5~")
        ui.pump(0.25)
        if time.monotonic() > deadline:
            ui.snap("FAIL-saved-code-not-reachable")
            raise AssertionError("saved assistant code was not reachable")
    ui.snap("history-03-code-color")
    code_row = next(
        i
        for i, line in enumerate(ui.screen.display)
        if "fn " + seed["code"].lower() in line
    )
    column = ui.screen.display[code_row].index("fn ")
    assert (
        ui.screen.buffer[code_row][column].fg == "94e2d5"
    ), "code blocks must be teal rather than white"
    checks.append("saved assistant code block renders in the theme code color")

    ui.send("\x1b[5~")
    ui.wait("READING")
    body = ui.screen.display[3:14]
    new_marker = seed["new"] + "_" + str(int(time.time()))
    api(
        "agent",
        "prompt",
        AGENT,
        f"Reply exactly {new_marker}. Do not use tools.",
        "--wait",
        "--timeout",
        "60000",
    )
    ui.wait("New output", 40)
    assert ui.screen.display[3:14] == body
    ui.snap("history-04-frozen-on-new-message")
    ui.send("\x1b[1;5F")
    ui.wait(new_marker)
    checks.append(
        "saved-history refresh does not move the reading viewport; returning live shows the new reply"
    )
finally:
    ui.close()

ui = UI()
try:
    ui.wait("Sonnet")
    ui.send("\x1b[17~")
    ui.wait("Conversation", 40)
    deadline = time.monotonic() + 60
    while seed["early"] not in ui.text():
        ui.send("\x1b[1;5H")
        ui.pump(0.5)
        if time.monotonic() > deadline:
            raise AssertionError("earliest saved turn was not reachable")
    ui.snap("history-05-client-reopen")
    checks.append(
        "closing and reopening the client reloads early history from Claude storage"
    )
finally:
    ui.close()


# Exercise the actual saved-machine route used by the desktop sidebar, with isolated
# catalog/selection files. No user profile or running local server is modified.
# UNIX domain sockets have a short path limit; keep the isolated runtime short.
profile_root = Path(tempfile.mkdtemp(prefix="hh-", dir="/tmp"))
profile_config = profile_root / "config.toml"
profile_config.write_text('onboarding = false\n[keys]\nprefix = "ctrl+a"\n')
env = dict(
    os.environ,
    HERDR_CONFIG_PATH=str(profile_config),
    XDG_CONFIG_HOME=str(profile_root / "cfg"),
    XDG_STATE_HOME=str(profile_root / "state"),
)
env.pop("HERDR_ENV", None)
subprocess.run(
    [
        BIN,
        "machine",
        "add",
        SSH,
        "--label",
        "history-e2e-profile",
        "--remote-session",
        SESSION,
    ],
    env=env,
    capture_output=True,
    text=True,
    timeout=40,
    check=True,
)
profiles = json.loads(
    subprocess.run(
        [BIN, "machine", "list", "--json"],
        env=env,
        capture_output=True,
        text=True,
        timeout=10,
        check=True,
    ).stdout
)
profile = next(p for p in profiles if p["label"] == "history-e2e-profile")
# Select only the fixture catalog entry, equivalent to its saved sidebar selection.
selection = next((profile_root / "state").glob("*/client/endpoints.json")).with_name(
    "endpoint-selection.json"
)
selection.write_text(json.dumps({"version": 1, "selected_profile": profile["id"]}))
local_session = "history-client-e2e-1028"
ui = UI(launch=[BIN, "--session", local_session], env_overrides=env)
try:
    ui.wait("Sonnet", 60)
    ui.send("\x1b[17~")
    ui.wait("Conversation", 40)
    deadline = time.monotonic() + 60
    while seed["early"] not in ui.text():
        ui.send("\x1b[1;5H")
        ui.pump(0.5)
        if time.monotonic() > deadline:
            raise AssertionError("earliest saved turn was not reachable")
    ui.snap("history-06-saved-machine-entry")
    checks.append(
        "saved-machine sidebar entry reads the selected remote conversation through its saved target/session"
    )
finally:
    ui.close()
    subprocess.run(
        [BIN, "--session", local_session, "server", "stop"],
        env=env,
        capture_output=True,
        timeout=20,
        check=False,
    )
    shutil.copytree(profile_root, OUT / "saved-machine", dirs_exist_ok=True)
    (OUT / "history-results.json").write_text(json.dumps(checks, indent=2))
    print(json.dumps(checks, indent=2))
