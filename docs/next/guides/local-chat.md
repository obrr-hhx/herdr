# Local chat and cached reader

In a recognized Claude Code pane, press **F6**, or choose **local chat / cached
reader** from the Herdr global menu. This is an optional client view; the remote
Claude process and terminal remain unchanged. Detach and launch the new Herdr
client after installing this feature; running servers do not need a restart. The target server must also advertise
`pane.read` and `agent.prompt` client-shell capabilities. Keep existing servers
and try the new version in a separate named session when those capabilities
are absent.

- Edit the draft locally, including Chinese text, paste, backspace, cursor movement,
  and multiline text. Enter submits the whole draft through the existing
  `agent.prompt` API. Ctrl+J, Shift+Enter, or Alt+Enter inserts a newline (terminal
  support for modified Enter varies).
- Scroll using the wheel, PageUp/PageDown, or Ctrl+Home/End. Scroll and edit actions
  do not wait for a server response. Ctrl+Y copies the entire cached text.
- Reading older content freezes the displayed snapshot. New output is cached
  separately until you return to the bottom using Ctrl+End or scroll down.
- Esc or F6 returns to the original terminal. Use it for permissions, interactive
  questions, slash-command completion, image paste, and interruption with Ctrl+C.
  The local draft is retained. One draft target is retained per client; clear or
  submit it before opening local chat for a different pane.

The reader caches real terminal text, not a reconstructed chat transcript. It
loads at most 2,000 retained terminal lines, capped to 1 MiB of UTF-8 text, and
refreshes changed terminal content at most twice per second, with one request in
flight. An application using the alternate screen may retain less history; the
reader cannot recover output that the remote terminal no longer holds. Cached
content is plain text; terminal colors and images remain in the original view.

Drafts are limited to 64 KiB and kept only in this client process's memory, not
saved to disk. Input is targeted to the selected endpoint, server boot identity,
and pane. An unconfirmed send restores the submitted text alongside any newer
draft and is never retried automatically. Inspect the original terminal first;
Ctrl+R explicitly permits a manual retry or reconnects a draft after a server
restart. Acknowledgement confirms terminal submission, not Claude's completion.

The view covers only the focused Claude pane, retaining tabs and neighbouring splits.
The bordered composer grows with the draft, and clicking its text moves the cursor.
Hints adapt to narrow panes; very small panes show an enlarge prompt. Host resize
redraws the local editor immediately without waiting for a remote frame and keeps
the current reading anchor. Wheel events scroll only the history area, and never
reach the remote terminal from inside the composer. Herdr prefix controls remain available.

Successive alternate-screen reads retain older text only when three complete lines
prove an overlap. If that evidence is missing, the reader shows **History limited**;
it does not invent missing output. Fast output between refreshes may still be lost.

The opt-in `scripts/local_chat_ssh_e2e.py` harness drives the real TUI through a PTY
and SSH against an already-running, disposable `local-chat-e2e-*` session and Claude
agent. It checks editing, eight host sizes, real prompt submission, reading during
new output, mouse scrolling, split focus and zoom. It isolates client config/state,
cleans up test splits, and saves terminal-cell PNG/text captures and JSON results.
Run its `--help` for required targets and Python dependencies. This is separate
from deterministic unit tests and requires an authenticated test Claude account.
