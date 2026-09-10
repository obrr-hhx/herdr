#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "Commit tracked changes before installing so the build ID identifies the source." >&2
    exit 1
fi

# The pinned toolchain and platform build dependencies are described in CONTRIBUTING.md.
export HERDR_BUILD_CHANNEL=obrr
export HERDR_BUILD_ID="$(git rev-parse --short=12 HEAD)"
just build

install_dir="${HERDR_FORK_BIN_DIR:-$HOME/.local/bin}"
backup_dir="${HERDR_FORK_BACKUP_DIR:-$HOME/.local/share/herdr-fork/backups}"
mkdir -p "$install_dir" "$backup_dir"
if [[ -e "$install_dir/herdr" || -L "$install_dir/herdr" ]]; then
    backup="$backup_dir/herdr-$(date -u +%Y%m%dT%H%M%SZ)-$$"
    cp -pL "$install_dir/herdr" "$backup"
    echo "Previous binary: $backup"
fi
staged="$(mktemp "$install_dir/.herdr-fork.XXXXXX")"
trap 'rm -f "$staged"' EXIT
install -m 755 "${CARGO_TARGET_DIR:-target}/release/herdr" "$staged"
mv -f "$staged" "$install_dir/herdr"
"$install_dir/herdr" --version
echo "Installed $install_dir/herdr"
echo "Detach and run herdr again to use the new client. Running servers and panes are preserved."
