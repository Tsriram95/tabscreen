#!/usr/bin/env bash
set -euo pipefail
systemctl --user disable --now tabscreen.service 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/tabscreen.service"
systemctl --user daemon-reload 2>/dev/null || true
rm -f "${PREFIX:-$HOME/.local}/bin/tabscreen-server"
rm -f "$HOME/.local/share/applications/tabscreen-server.desktop"
echo "Removed the TabScreen server, service and desktop registration."
echo "To also revoke /dev/uinput access: sudo rm /etc/udev/rules.d/70-tabscreen-uinput.rules"
