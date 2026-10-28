# Local input and conversation history

Press **F6** in a recognized Claude Code pane, or select **local chat** from the global menu.
Editing and history scrolling run locally while the remote Claude process continues running.
After installing a client update, detach and reopen the client; existing remote servers need not restart.

- **Enter** submits the whole message; **Ctrl+J** inserts a newline. Shift/Alt+Enter depend on the host terminal.
- Wheel and **PageUp/PageDown** scroll the local history. **Ctrl+Home/End** moves to the start of loaded
  history or the latest output. Continue upward at the start to fetch earlier records; **LOADING** indicates a page read.
- Reading stays fixed while new replies arrive. Returning to the bottom or Ctrl+End resumes live following.
- Click draft text to position the cursor. The bordered composer grows with the draft; Herdr prefix controls remain available.
- **Ctrl+Y** copies loaded history. **Esc/F6** returns to the original terminal for approvals, slash completion,
  images and Ctrl+C cancellation, retaining the draft.

History comes from the selected Claude process's saved conversation, including messages saved before
this client opened. Binding uses the current endpoint, pane and actual process session; it never guesses
from a project's newest file. Diagnostic API request/response copies, internal context and sidechain logs
are not shown as main conversation turns. User messages, Claude replies, code blocks, tool results and
errors use the current theme's semantic colors. This is a structured conversation view, not a pixel-perfect
copy of the terminal's ANSI appearance.

Reads run in the background for only the active pane, normally at most once per second. Unchanged
files do not retransmit content. Each page scans at most 2 MiB of logs and returns 1 MiB of text. The
local window retains at most 8 MiB or 10,000 content blocks; earlier pages remain browsable after the
window fills. Tool results display at most 64 KiB, and oversized content blocks are explicitly marked.
Images use placeholders; view originals in the terminal. Drafts are limited to 64 KiB.

Unsaved output appears separately in a small **Live terminal** preview and never enters saved history.
The preview hides while reading upward. An agent needing confirmation shows **Needs attention · Esc terminal**.
Unavailable history is explicit and retains the original terminal entry point. The fallback screen cache
is limited to 2,000 lines / 1 MiB and does not claim to contain the whole conversation. Drafts and caches
stay in client memory; no extra conversation file is written.

The read-only provider uses Python 3.9+ standard libraries on the source host, resolves the actual Claude
process through Herdr's public API, and verifies its session metadata and process-start identity. Claude
Code 2.1.311's format has been verified; unsupported formats fail closed. SSH reads reuse the existing
target, identity and strict host-key checks, without installing remote dependencies or restarting servers.
The provider is also independently available:

```sh
herdr --session NAME agent history PANE_ID --ssh-target SSH_TARGET
herdr --session NAME agent history PANE_ID --ssh-target SSH_TARGET --before BYTE_OFFSET
```

Submissions bind to the endpoint, server boot and pane. Unconfirmed submissions restore both the sent
text and newer draft, and never retry automatically. Inspect the terminal with Esc, then explicitly allow
manual retry with Ctrl+R. Acknowledgement confirms submission, not reply completion. One client retains
one draft target; send or clear it before targeting a different pane.

The optional `scripts/local_chat_ssh_e2e.py` harness exercises real PTY/SSH/Claude editing, sizes,
scrolling, splits and zoom. `scripts/local_chat_history_e2e.py` additionally checks pre-existing early
messages, saved content beyond 2,000 lines, theme colors, new replies while reading, and client reopen.
Use only an isolated `local-chat-e2e-*` or `local-chat-history-e2e-*` test session. Scripts retain real
terminal-cell PNG/text captures and JSON results; see `--help` for dependencies and arguments.

The submitted text appears immediately in a separate local message with a sending indicator. A remote acknowledgement changes it to sent; the saved user record replaces it without a duplicate. Unconfirmed sends return to the draft and are never automatically retried. Up to 16 messages may await history synchronization; beyond that the draft is retained.

Live terminal previews reuse already-received surface cells instead of sharing the command queue with passive reads. Saved-history polling temporarily runs up to four times per second while a submission awaits synchronization, with only one read in flight. Remote submission still includes the agent paste/Enter delay and network transit; local echo is not evidence of a Claude reply.

`python3 scripts/local_chat_preview_e2e.py --help` describes the optional real-SSH checks for cached switching, rapid retargeting, local echo, acknowledgement timing, and reconciliation. Supply a dedicated test session with a ready Claude test agent.
