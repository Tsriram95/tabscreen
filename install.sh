#!/usr/bin/env bash
# TabScreen server installer for KDE Plasma (Wayland).
# Builds the server, installs it as a per-user service, and grants /dev/uinput access.
#
#   curl -fsSL https://raw.githubusercontent.com/Tsriram95/tabscreen/main/install.sh | bash
# or, from a clone:
#   ./install.sh
set -euo pipefail

BOLD=$'\e[1m'; RED=$'\e[31m'; GRN=$'\e[32m'; YEL=$'\e[33m'; RST=$'\e[0m'
info(){ echo "${GRN}==>${RST} $*"; }
warn(){ echo "${YEL}warning:${RST} $*" >&2; }
die(){ echo "${RED}error:${RST} $*" >&2; exit 1; }

REPO_URL="${TABSCREEN_REPO:-https://github.com/Tsriram95/tabscreen}"
PREFIX="${PREFIX:-$HOME/.local}"
BIN="$PREFIX/bin/tabscreen-server"

# --- 0. sanity checks -------------------------------------------------------
[ "$(id -u)" -ne 0 ] || die "run as your normal user, not root (it uses your KDE session and installs a --user service)."
command -v kwin_wayland >/dev/null 2>&1 || warn "kwin_wayland not found — TabScreen needs KDE Plasma on Wayland."
[ "${XDG_SESSION_TYPE:-}" = wayland ] || warn "not a Wayland session right now (\$XDG_SESSION_TYPE=${XDG_SESSION_TYPE:-unset}); the server only works under KDE Wayland."

# --- 1. dependencies --------------------------------------------------------
declare -A PKGS_PACMAN=(
  [rust]="rustup" [cc]="base-devel" [wl]="wayland" [gst]="gstreamer gst-plugins-base gst-plugins-good gst-plugin-pipewire gst-plugins-bad gst-plugin-va" [va]="libva-mesa-driver libva-utils" [pa]="libpulse"
)
install_deps(){
  if command -v pacman >/dev/null; then
    info "installing dependencies with pacman (sudo)…"
    sudo pacman -S --needed --noconfirm rustup base-devel wayland pkgconf \
      gstreamer gst-plugins-base gst-plugins-good gst-plugin-pipewire gst-plugins-bad gst-plugin-va \
      libva-utils libpulse || die "pacman failed"
    # A Mesa VA-API driver for your GPU (AMD/Intel). NVIDIA users: install libva-nvidia-driver.
    sudo pacman -S --needed --noconfirm libva-mesa-driver 2>/dev/null || true
  elif command -v apt-get >/dev/null; then
    info "installing dependencies with apt (sudo)…"
    sudo apt-get update
    sudo apt-get install -y build-essential pkg-config libwayland-dev \
      libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
      gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-pipewire gstreamer1.0-vaapi \
      libpulse0 vainfo curl || die "apt failed"
    command -v rustup >/dev/null || curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
  elif command -v dnf >/dev/null; then
    info "installing dependencies with dnf (sudo)…"
    sudo dnf install -y @development-tools pkgconf-pkg-config wayland-devel rust cargo \
      gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-good gstreamer1-plugins-bad-free \
      pipewire-gstreamer libva-utils pulseaudio-libs || die "dnf failed"
  else
    warn "unknown distro — install manually: rust, a C toolchain, wayland dev headers, GStreamer (base/good/bad + pipewire + va plugins), a VA-API driver, libpulse."
  fi
  command -v rustup >/dev/null && rustup default stable >/dev/null 2>&1 || true
  [ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env" || true
}

# --- 2. get the source ------------------------------------------------------
if [ -f "$(dirname "$0")/server/Cargo.toml" ]; then
  SRC="$(cd "$(dirname "$0")" && pwd)"
  info "building from local checkout at $SRC"
else
  SRC="${TMPDIR:-/tmp}/tabscreen-src"
  info "cloning $REPO_URL"
  rm -rf "$SRC"; git clone --depth 1 "$REPO_URL" "$SRC"
fi

install_deps

# --- 3. build ---------------------------------------------------------------
info "building the server (this can take a few minutes the first time)…"
( cd "$SRC/server" && cargo build --release )
mkdir -p "$PREFIX/bin"
install -m755 "$SRC/server/target/release/tabscreen-server" "$BIN"
info "installed $BIN"

# --- 4. /dev/uinput access --------------------------------------------------
if [ ! -w /dev/uinput ]; then
  info "granting access to /dev/uinput (sudo)…"
  sudo install -m644 "$SRC/server/contrib/70-tabscreen-uinput.rules" /etc/udev/rules.d/70-tabscreen-uinput.rules
  sudo udevadm control --reload
  sudo udevadm trigger --name-match=uinput || true
  if ! id -nG | grep -qw input; then
    sudo usermod -aG input "$USER"
    warn "added you to the 'input' group — log out and back in (or reboot) for it to take effect."
  fi
fi

# --- 4b. firewall -----------------------------------------------------------
open_firewall(){
  # TCP 7741 (stream + TCP discovery) and UDP 7742 (broadcast discovery).
  if systemctl is-active --quiet firewalld 2>/dev/null && command -v firewall-cmd >/dev/null; then
    info "opening ports in firewalld (sudo)…"
    sudo firewall-cmd --permanent --add-port=7741/tcp --add-port=7742/udp >/dev/null 2>&1 || true
    sudo firewall-cmd --reload >/dev/null 2>&1 || true
  fi
  if command -v ufw >/dev/null && sudo ufw status 2>/dev/null | grep -qi active; then
    info "opening ports in ufw (sudo)…"
    sudo ufw allow 7741/tcp >/dev/null 2>&1 || true
    sudo ufw allow 7742/udp >/dev/null 2>&1 || true
  fi
}
open_firewall

# --- 5. user service --------------------------------------------------------
UNIT_DIR="$HOME/.config/systemd/user"
mkdir -p "$UNIT_DIR"
sed "s|__BIN__|$BIN|" "$SRC/server/contrib/tabscreen.service" > "$UNIT_DIR/tabscreen.service"
systemctl --user daemon-reload
systemctl --user enable --now tabscreen.service 2>/dev/null || warn "could not start the service now (are you in a graphical session?); it will start on next login."

echo
info "${BOLD}Done.${RST}"
echo "  • Server binary : $BIN"
echo "  • Service       : systemctl --user status tabscreen"
echo "  • Logs          : journalctl --user -u tabscreen -f"
echo "  • Listening on  : port 7741 (all interfaces)"
IP=$(ip -4 -o addr show scope global 2>/dev/null | awk '{print $4}' | cut -d/ -f1 | head -1)
[ -n "${IP:-}" ] && echo "  • This machine  : $IP  (enter this in the tablet app; port 7741)"
echo
echo "Install the Android app from the GitHub Releases page and connect."
if ! id -nG | grep -qw input && [ ! -w /dev/uinput ]; then
  echo "${YEL}Reminder: log out/in once so pen & touch input work (input group).${RST}"
fi
