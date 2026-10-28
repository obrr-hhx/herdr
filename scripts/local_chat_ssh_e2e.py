#!/usr/bin/env python3
"""Drive the real TUI through a PTY and SSH, retain terminal-cell captures, and clean up test splits.

The session and Claude agent must already exist. This sends small test prompts to
that account. Captures are rasterized terminal cells, not OS font screenshots.
The output directory also isolates client config/state from running user sessions.
"""
import os, pty, fcntl, termios, struct, subprocess, select, time, json, codecs, signal, shlex, argparse, tempfile
from pathlib import Path
import pyte
from PIL import Image, ImageDraw, ImageFont

parser = argparse.ArgumentParser(
    description="Real SSH/Claude local-chat acceptance; requires an isolated test server and agent. Python dependencies: pyte==0.8.2 pillow==12.0.0."
)
parser.add_argument(
    "--binary", default=str(Path(__file__).resolve().parents[1] / "target/debug/herdr")
)
parser.add_argument("--ssh-target", required=True)
parser.add_argument("--remote-binary", default="herdr")
parser.add_argument("--session", required=True)
parser.add_argument("--agent", default="local-chat-e2e")
parser.add_argument("--output-dir", default=None)
parser.add_argument("--font", default="/System/Library/Fonts/Menlo.ttc")
args = parser.parse_args()
if not args.session.startswith(("local-chat-e2e-", "local-chat-history-e2e-")):
    parser.error(
        "use a dedicated local-chat-e2e-* session; never target a user session"
    )
OUT = Path(
    args.output_dir or tempfile.mkdtemp(prefix="herdr-local-chat-e2e-")
).resolve()
OUT.mkdir(parents=True, exist_ok=True)
(OUT / "config.toml").write_text('onboarding = false\n[keys]\nprefix = "ctrl+a"\n')
BIN = str(Path(args.binary).resolve())
SSH = args.ssh_target
REMOTE = args.remote_binary
SESSION = args.session
AGENT = args.agent
RUN_ID = f"CACHE_{int(time.time()):x}"
NEW_OUTPUT = f"{RUN_ID}_NEW_OK"
checks = []
created_panes = []


def api(*args):
    r = subprocess.run(
        [
            "ssh",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=10",
            SSH,
            shlex.join([REMOTE, "--session", SESSION, *args]),
        ],
        capture_output=True,
        text=True,
        timeout=90,
    )
    if r.returncode:
        raise AssertionError(r.stderr + r.stdout)
    return r.stdout


class Screen(pyte.Screen):
    def report_device_status(self, mode, **kwargs):
        pass

    def set_title(self, title):
        pass


class Child:
    def __init__(self, pid):
        self.pid = pid
        self.done = False

    def poll(self):
        if self.done:
            return 0
        pid, status = os.waitpid(self.pid, os.WNOHANG)
        if pid:
            self.done = True
            return status
        return None

    def terminate(self):
        os.kill(self.pid, signal.SIGTERM)


class UI:
    def __init__(self, launch=None, env_overrides=None):
        self.cols, self.rows = 160, 48
        self.screen = Screen(self.cols, self.rows)
        self.stream = pyte.Stream(self.screen)
        self.decode = codecs.getincrementaldecoder("utf8")("replace")
        env = dict(
            os.environ,
            TERM="xterm-256color",
            HERDR_CONFIG_PATH=str(OUT / "config.toml"),
            XDG_CONFIG_HOME=str(OUT / "cfg"),
            XDG_STATE_HOME=str(OUT / "state"),
        )
        env.pop("HERDR_ENV", None)
        if env_overrides:
            env.update(env_overrides)
        pid, self.master = pty.fork()
        if pid == 0:
            os.execvpe(BIN, launch or [BIN, "--remote", SSH, "--session", SESSION], env)
        self.p = Child(pid)
        fcntl.ioctl(
            self.master,
            termios.TIOCSWINSZ,
            struct.pack("HHHH", self.rows, self.cols, 0, 0),
        )

    def pump(self, seconds=0.3):
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            if select.select([self.master], [], [], 0.02)[0]:
                data = os.read(self.master, 1024 * 1024)
                self.stream.feed(self.decode.decode(data))

    def send(self, data):
        os.write(self.master, data.encode() if isinstance(data, str) else data)
        self.pump()

    def text(self):
        return "\n".join(self.screen.display)

    def wait(self, text, timeout=40):
        end = time.monotonic() + timeout
        while text not in self.text():
            self.pump(0.1)
            if time.monotonic() > end:
                self.snap("FAIL-" + text[:20])
                raise AssertionError("not visible: " + text + "\n" + self.text())

    def wait_absent(self, text, timeout=20):
        deadline = time.monotonic() + timeout
        while text in self.text():
            self.pump(0.1)
            if time.monotonic() > deadline:
                self.snap("FAIL-absent-" + text[:20])
                raise AssertionError("still visible: " + text)

    def resize(self, cols, rows):
        self.cols, self.rows = cols, rows
        self.screen.resize(lines=rows, columns=cols)
        fcntl.ioctl(
            self.master, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0)
        )
        os.killpg(self.p.pid, signal.SIGWINCH)
        self.pump(1)

    def snap(self, name):
        (OUT / (name + ".txt")).write_text(self.text())
        font = ImageFont.truetype(args.font, 16)
        cw, ch = 10, 22
        im = Image.new("RGB", (self.cols * cw + 24, self.rows * ch + 24), "#181825")
        d = ImageDraw.Draw(im)
        for y in range(self.rows):
            for x in range(self.cols):
                c = self.screen.buffer[y][x]

                def color(v, default):
                    if len(v) == 6:
                        try:
                            int(v, 16)
                            return "#" + v
                        except ValueError:
                            pass
                    return {
                        "black": "#181825",
                        "white": "#cdd6f4",
                        "blue": "#89b4fa",
                        "green": "#a6e3a1",
                        "yellow": "#f9e2af",
                        "red": "#f38ba8",
                    }.get(v, default)

                bg = color(c.bg, "#181825")
                fg = color(c.fg, "#cdd6f4")
                d.rectangle(
                    (12 + x * cw, 12 + y * ch, 12 + (x + 1) * cw, 12 + (y + 1) * ch),
                    fill=bg,
                )
                if c.data:
                    d.text((12 + x * cw, 12 + y * ch), c.data, font=font, fill=fg)
        im.save(OUT / (name + ".png"))

    def close(self):
        self.send("\x1b")
        self.send("\x01d")
        self.pump(0.3)
        if self.p.poll() is None:
            self.p.terminate()
        os.close(self.master)


def main():
    ui = UI()
    try:
        ui.wait("Sonnet")
        ui.send("\x1b[17~")
        ui.wait("CLAUDE")
        ui.pump(1)
        ui.snap("01-wide")
        checks.append("actual SSH preview, F6 opens and history loads")
        ui.send("\x1b[200~中文输入与本地编辑\nsecond line\x1b[201~")
        ui.snap("02-multiline")
        assert "second line" in ui.text()
        row = next(
            n for n, line in enumerate(ui.screen.display) if "second line" in line
        )
        col = ui.screen.display[row].index("second line")
        ui.send(f"\x1b[<0;{col+3};{row+1}M")
        ui.send("X")
        assert "seXcond line" in ui.text()
        ui.send("\x7f")
        ui.snap("02-mouse-edit")
        checks.append("mouse click places editor caret and edits at clicked position")
        raw = api(
            "agent", "read", AGENT, "--source", "recent-unwrapped", "--lines", "50"
        )
        assert "中文输入与本地编辑" not in raw
        checks.append("Chinese multiline bracketed paste remains local before Enter")
        for cols, rows in [
            (106, 24),
            (80, 24),
            (60, 18),
            (42, 12),
            (30, 9),
            (20, 7),
            (10, 4),
            (160, 48),
        ]:
            ui.resize(cols, rows)
            ui.snap(f"03-resize-{cols}x{rows}")
            assert ui.screen.cursor.x < cols and ui.screen.cursor.y < rows
        checks.append(
            "eight resize sizes including tiny window, draft restored on enlarge"
        )
        assert "second line" in ui.text()
        ui.send("\x15")
        ui.send(
            f'Reply with exactly 60 numbered lines, each formatted "{RUN_ID}_01 local reading test", incrementing from 01 to 60. No tools, no code fences, no other prose.'
        )
        ui.send("\r")
        ui.wait(f"{RUN_ID}_60", 90)
        ui.pump(1)
        ui.snap("04-live-output")
        checks.append(
            "local composer submits to real Claude and caches streamed response"
        )
        ui.resize(106, 24)
        ui.send("\x1b[5~")
        ui.wait("READING")
        ui.snap("05-page-up")
        top = [line[28:-3] for line in ui.screen.display[3:14]]
        api(
            "agent",
            "prompt",
            AGENT,
            f"Reply exactly {NEW_OUTPUT}. Do not use tools.",
            "--wait",
            "--timeout",
            "60000",
        )
        ui.wait("New output")
        ui.snap("06-reading-frozen-new-output")
        assert [line[28:-3] for line in ui.screen.display[3:14]] == top
        checks.append("reading remains frozen while real new Claude output arrives")
        ui.send("\x1b[1;5F")
        ui.wait(NEW_OUTPUT)
        ui.snap("07-return-live")
        checks.append("Ctrl+End resumes latest cached output")
        # Wheel packets must update local reader rather than moving remote Claude viewport.
        ui.send("\x1b[<64;50;10M")
        ui.wait("READING")
        ui.snap("08-wheel-up")
        ui.send("\x1b[<65;50;10M")
        ui.send("\x1b[1;5F")
        checks.append("SGR mouse wheel up/down works in local reader")
        ui.send("\x01")
        ui.wait("PREFIX")
        ui.send("\x1b")
        ui.wait("CLAUDE")
        checks.append("prefix controls remain accessible inside local view")
        ui.snap("09-final")
        ui.resize(160, 48)

        # Exercise split geometry and focus against actual remote PTYs, never the user's session.
        def new_pane(response):
            found = []

            def walk(value):
                if isinstance(value, dict):
                    if "pane_id" in value and value["pane_id"] != "w1:p1":
                        found.append(value["pane_id"])
                    for child in value.values():
                        walk(child)
                elif isinstance(value, list):
                    for child in value:
                        walk(child)

            walk(json.loads(response))
            return found[0]

        right = new_pane(
            api("pane", "split", "w1:p1", "--direction", "right", "--no-focus")
        )
        created_panes.append(right)
        api("pane", "run", right, "printf '\\nNEIGHBOUR_RIGHT_OK\\n'")
        ui.wait("NEIGHBOUR_RIGHT_OK")
        ui.wait("CLAUDE")
        ui.snap("10-split-right")
        checks.append(
            "horizontal split keeps neighbour terminal visible beside local reader"
        )
        api(
            "pane",
            "resize",
            "--pane",
            "w1:p1",
            "--direction",
            "right",
            "--amount",
            "0.1",
        )
        ui.pump(2)
        ui.snap("11-split-resize")
        row = next(
            n
            for n, line in enumerate(ui.screen.display)
            if "NEIGHBOUR_RIGHT_OK" in line
        )
        col = ui.screen.display[row].index("NEIGHBOUR_RIGHT_OK")
        ui.send(f"\x1b[<0;{col+1};{row+1}M")
        ui.send(f"\x1b[<0;{col+1};{row+1}m")
        ui.pump(1)
        ui.wait_absent("CLAUDE")
        ui.snap("11-clicked")
        api("pane", "focus", "--pane", right, "--direction", "left")
        ui.wait("CLAUDE")
        checks.append(
            "mouse switches adjacent pane and returning restores local reader"
        )
        down = new_pane(
            api("pane", "split", "w1:p1", "--direction", "down", "--no-focus")
        )
        created_panes.append(down)
        api("pane", "run", down, "printf '\\nNEIGHBOUR_DOWN_OK\\n'")
        ui.wait("NEIGHBOUR_DOWN_OK")
        ui.wait("NEIGHBOUR_RIGHT_OK")
        ui.wait("CLAUDE")
        ui.snap("12-split-down")
        api("pane", "zoom", "w1:p1", "--on")
        ui.pump(2)
        ui.wait("CLAUDE")
        ui.wait_absent("NEIGHBOUR_DOWN_OK")
        ui.snap("13-zoom")
        api("pane", "zoom", "w1:p1", "--off")
        ui.pump(2)
        ui.wait("NEIGHBOUR_DOWN_OK")
        ui.snap("14-unzoom")
        checks.append(
            "vertical split and zoom/unzoom preserve local editor and both neighbour panes"
        )
    finally:
        ui.close()
        for pane in reversed(created_panes):
            api("pane", "close", pane)
        (OUT / "results.json").write_text(
            json.dumps(checks, ensure_ascii=False, indent=2)
        )
        print(json.dumps(checks, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
