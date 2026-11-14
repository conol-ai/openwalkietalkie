#!/bin/sh
# Install the Omarchy bridge as systemd user services.
#   install.sh [path/to/deckd]     the Linux deckd binary (default: ~/.local/bin/deckd as it is)
# Idempotent; `systemctl --user disable --now deckd omarchy-deck` undoes it.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
data="$HOME/.local/share/openwalkietalkie"
share="$data/omarchy"
units="$HOME/.config/systemd/user"
rule=/etc/udev/rules.d/70-openwalkietalkie.rules
mkdir -p "$share" "$units" "$HOME/.local/bin"
cp "$here"/omarchy_*.py "$share"/
if [ "${1:-}" ]; then install -m 755 "$1" "$HOME/.local/bin/deckd"; fi
[ -x "$HOME/.local/bin/deckd" ] || { echo "no deckd at ~/.local/bin/deckd: pass the binary as the first argument" >&2; exit 1; }
cp "$here"/systemd/*.service "$units"/
systemctl --user daemon-reload
systemctl --user enable --now deckd.service omarchy-deck.service
# A second run is an update: the units already run the old code, and `enable --now` leaves them be.
if [ "${1:-}" ]; then systemctl --user restart deckd.service; fi
systemctl --user restart omarchy-deck.service

# The bridge is what puts this machine's sessions on the deck (its hooks, omarchy_hook.py).
# deckd's own Claude source (its hooks and the `claude agents` roster seed) and its Codex
# app-server connection would register the same sessions a second time, under other keys.
i=0
until DECKD_DATA_DIR="$data" "$HOME/.local/bin/deckd" ctl session.ping >/dev/null 2>&1; do
    i=$((i + 1))
    [ "$i" -lt 50 ] || { echo "deckd did not answer on its socket: run install.sh again" >&2; exit 1; }
    sleep 0.2
done
DECKD_DATA_DIR="$data" "$HOME/.local/bin/deckd" config set sources.claude=false codex.daemon=false >/dev/null

systemctl --user --no-pager --lines=0 status deckd.service omarchy-deck.service || true

# Without the udev rule, /dev/hidraw* is root's and deckd cannot open the deck.
if ! grep -qs '1209:5754' "$rule"; then
    echo
    echo "One step needs root: let deckd open the deck (then it links by itself):"
    echo "  sudo install -m 644 $here/70-openwalkietalkie.rules $rule"
    echo "  sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=hidraw"
fi
