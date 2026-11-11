#!/bin/sh
# Install the Omarchy bridge as systemd user services.
#   install.sh [path/to/deckd]     the Linux deckd binary (default: ~/.local/bin/deckd as it is)
# Idempotent; `systemctl --user disable --now deckd omarchy-deck` undoes it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
share="$HOME/.local/share/openwalkietalkie/omarchy"
units="$HOME/.config/systemd/user"
mkdir -p "$share" "$units" "$HOME/.local/bin"
cp "$here"/omarchy_*.py "$share"/
if [ "${1:-}" ]; then install -m 755 "$1" "$HOME/.local/bin/deckd"; fi
[ -x "$HOME/.local/bin/deckd" ] || { echo "no deckd at ~/.local/bin/deckd: pass the binary as the first argument" >&2; exit 1; }
cp "$here"/systemd/*.service "$units"/
systemctl --user daemon-reload
systemctl --user enable --now deckd.service omarchy-deck.service
systemctl --user --no-pager --lines=0 status deckd.service omarchy-deck.service || true
