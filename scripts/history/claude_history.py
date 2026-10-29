"""Bound a Claude transcript page to a live Herdr pane. JSON data only; no terminal input."""

import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys

WINDOW = 2 * 1024 * 1024
MAX_TEXT = 1024 * 1024
BLOCK_TEXT = 65536


_API_SOCKET = None


def api(binary, session, *args):
    global _API_SOCKET
    if os.name != "nt":
        if _API_SOCKET is None:
            status = subprocess.run(
                [binary, "--session", session, "status", "server"],
                capture_output=True,
                timeout=5,
                check=False,
            )
            paths = [
                line[8:]
                for line in status.stdout.decode("utf-8", "replace").splitlines()
                if line.startswith("socket: ")
            ]
            if status.returncode or len(paths) != 1:
                raise ValueError("Herdr API socket is unavailable")
            _API_SOCKET = paths[0]
        method = "pane.get" if args[1] == "get" else "pane.process_info"
        pane = args[2] if method == "pane.get" else args[3]
        request = {
            "id": "history-provider",
            "method": method,
            "params": {"pane_id": pane},
        }
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(5)
            stream.connect(_API_SOCKET)
            stream.sendall((json.dumps(request) + "\n").encode())
            with stream.makefile("rb") as reader:
                value = json.loads(reader.readline(2 * 1024 * 1024))
    else:
        result = subprocess.run(
            [binary, "--session", session, *args],
            capture_output=True,
            timeout=5,
            check=False,
        )
        if result.returncode:
            raise ValueError("Herdr pane query failed")
        value = json.loads(result.stdout)
    if "error" in value:
        raise ValueError("Herdr pane query rejected")
    return value["result"]


def clean(text):
    text = re.sub(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)", "", text)
    text = re.sub(r"\x1b\[[0-?]*[ -/]*[@-~]", "", text)
    return "".join(
        ch
        for ch in text
        if ch in "\n\t"
        or ord(ch) >= 32
        and ord(ch) != 127
        and not 0xD800 <= ord(ch) <= 0xDFFF
    )


def blocks(record, offset):
    # Request/response blobs contain internal context and copies of earlier messages.
    # They are diagnostics, not transcript turns, even when message.role is "user".
    if record.get("type") not in ("user", "assistant"):
        return []
    message = record.get("message", {})
    if not isinstance(message, dict):
        return []
    role = message.get("role")
    if (
        role not in ("user", "assistant")
        or record.get("isMeta")
        or record.get("isSidechain")
    ):
        return []
    content = message.get("content", [])
    if isinstance(content, str):
        content = [{"type": "text", "text": content}]
    if not isinstance(content, list):
        return []
    output = []
    for index, block in enumerate(content):
        if not isinstance(block, dict):
            continue
        kind = block.get("type")
        if kind == "text":
            text, style = block.get("text", ""), role
        elif kind == "tool_use":
            text, style = block.get("name", "Tool"), "tool_call"
        elif kind == "tool_result":
            result = block.get("content", "")
            text = (
                result
                if isinstance(result, str)
                else "\n".join(
                    b.get("text", "")
                    for b in result
                    if isinstance(b, dict) and b.get("type") == "text"
                )
            )
            style = "error" if block.get("is_error") else "tool_result"
        elif kind == "image":
            text, style = "[image — open the original terminal to view]", "tool_result"
        else:
            continue
        if not isinstance(text, str) or not text.strip():
            continue
        limit = BLOCK_TEXT if style in ("tool_result", "error") else MAX_TEXT - 256
        encoded = clean(text).encode("utf-8")
        clipped = len(encoded) > limit
        text = encoded[:limit].decode("utf-8", "ignore")
        if clipped:
            text += "\n[… this block exceeds the history text limit …]"
        identity = str(record.get("uuid") or f"offset:{offset}")
        output.append(
            {"id": f"{identity}:{index}", "offset": offset, "kind": style, "text": text}
        )
    return output


def page(path, before=None):
    size = path.stat().st_size
    end = size if before is None else min(max(before, 0), size)
    start = max(0, end - WINDOW)
    with path.open("rb") as stream:
        stream.seek(start)
        if start:
            stream.readline(end - start)
        start = stream.tell()
        data = stream.read(end - start)
    records, total = [], 0
    cursor = start
    for line in data.splitlines(keepends=True):
        position = cursor
        cursor += len(line)
        if not line.endswith(b"\n"):
            break  # A concurrently appended partial JSON record is retried next refresh.
        try:
            value = json.loads(line)
        except (ValueError, UnicodeError):
            continue
        if not isinstance(value, dict):
            continue
        for block in blocks(value, position):
            records.append(block)
            total += len(block["text"].encode("utf-8"))
        while total > MAX_TEXT and records:
            removed = records.pop(0)
            total -= len(removed["text"].encode("utf-8"))
            start = records[0]["offset"] if records else cursor
    if start >= end and end > 0:
        start = max(
            0, end - WINDOW
        )  # An oversized/partial record must not create a cursor loop.
    return {"records": records, "before": start, "has_older": start > 0}


def resolve(root, info):
    candidates = []
    for process in info.get("foreground_processes", []):
        pid = process.get("pid")
        if not isinstance(pid, int) or pid <= 0:
            continue
        path = root / "sessions" / f"{pid}.json"
        if not path.is_file() or not path.resolve().is_relative_to(root.resolve()):
            continue
        metadata = json.loads(path.read_text())
        if metadata.get("pid") != pid:
            continue
        stat_path = Path(f"/proc/{pid}/stat")
        if stat_path.exists():
            fields = stat_path.read_text().rsplit(")", 1)[1].split()
            if str(metadata.get("procStart", "")) != fields[19]:
                continue  # PID reuse must not bind an old transcript to a new process.
        identity = metadata.get("sessionId", "")
        if not isinstance(identity, str) or not re.fullmatch(
            r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}", identity
        ):
            continue
        cwd = metadata.get("cwd")
        if not isinstance(cwd, str) or not Path(cwd).is_absolute():
            continue
        project = re.sub(r"[^a-zA-Z0-9]", "-", cwd)
        transcript = root / "projects" / project / f"{identity}.jsonl"
        if transcript.is_file() and transcript.resolve().is_relative_to(
            (root / "projects").resolve()
        ):
            candidates.append((identity, transcript))
    if len(candidates) != 1:
        raise ValueError("Cannot uniquely bind this pane to a live Claude session")
    return candidates[0]


def run(session, pane, before, known, binary=None, api_socket=None):
    global _API_SOCKET
    if api_socket:
        _API_SOCKET = api_socket
    binary = binary or shutil.which("herdr") or str(Path.home() / ".local/bin/herdr")
    first = api(binary, session, "pane", "get", pane)["pane"]
    if first.get("agent") != "claude":
        raise ValueError("The selected pane is not Claude Code")
    info = api(binary, session, "pane", "process-info", "--pane", pane)["process_info"]
    root = Path(
        os.environ.get("CLAUDE_CONFIG_DIR", str(Path.home() / ".claude"))
    ).expanduser()
    identity, path = resolve(root, info)
    stat = path.stat()
    revision = f"{first['terminal_id']}:{identity}:{stat.st_ino}:{stat.st_size}:{stat.st_mtime_ns}"
    result = {
        "version": 1,
        "session_id": identity,
        "terminal_id": first["terminal_id"],
        "revision": revision,
        "unchanged": before is None and revision == known,
        "records": [],
        "before": 0,
        "has_older": False,
    }
    if not result["unchanged"]:
        result.update(page(path, before))
    last = api(binary, session, "pane", "get", pane)["pane"]
    if first["terminal_id"] != last["terminal_id"]:
        raise ValueError("The pane changed while history was being read")
    current = api(binary, session, "pane", "process-info", "--pane", pane)[
        "process_info"
    ]
    if resolve(root, current)[0] != identity:
        raise ValueError("The Claude session changed while history was being read")
    return result


def main():
    try:
        session, pane, cursor, known = sys.argv[1:5]
        result = run(
            session,
            pane,
            None if cursor == "latest" else int(cursor),
            known,
            *(sys.argv[5:7]),
        )
        print(json.dumps(result, ensure_ascii=False))
    except Exception as error:
        # Never echo subprocess output, file content, or credentials on an error path.
        message = (
            str(error)
            if isinstance(error, ValueError)
            else "Claude history is unavailable"
        )
        print(json.dumps({"error": message}))
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
