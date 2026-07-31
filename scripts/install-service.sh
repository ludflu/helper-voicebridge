#!/bin/bash
# Install qc-voice-bridge as a user systemd service on the Pi.
# Run on lilhelper: ~/helper-agent/install-service.sh
#
# - Enables user linger (starts at boot without login)
# - Passwordless sudo for qc-voice-bridge only (btmon)
# - Enables and starts the service

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
USER_NAME="${SUDO_USER:-$(whoami)}"
HOME_DIR="$(eval echo "~${USER_NAME}")"
AGENT_DIR="${HOME_DIR}/helper-agent"
BRIDGE="${AGENT_DIR}/qc-voice-bridge"
SERVICE_NAME="qc-voice-bridge.service"
SERVICE_SRC="${SCRIPT_DIR}/${SERVICE_NAME}"
USER_UNIT_DIR="${HOME_DIR}/.config/systemd/user"
SUDOERS="/etc/sudoers.d/qc-voice-bridge"
USER_UID="$(id -u "$USER_NAME")"
export_user_systemctl() {
  sudo -u "$USER_NAME" \
    XDG_RUNTIME_DIR="/run/user/${USER_UID}" \
    DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/${USER_UID}/bus" \
    "$@"
}

if [[ "$(id -u)" -ne 0 ]]; then
  echo "Run with sudo: sudo ${AGENT_DIR}/install-service.sh" >&2
  exit 1
fi

if [[ ! -x "$BRIDGE" ]]; then
  echo "Missing executable: $BRIDGE" >&2
  exit 1
fi

if [[ ! -x "${AGENT_DIR}/run.sh" ]]; then
  echo "Missing ${AGENT_DIR}/run.sh" >&2
  exit 1
fi

if [[ ! -f "$SERVICE_SRC" ]]; then
  echo "Missing ${SERVICE_SRC}" >&2
  exit 1
fi

install -d -o "$USER_NAME" -g "$USER_NAME" "$USER_UNIT_DIR"
install -o "$USER_NAME" -g "$USER_NAME" -m 644 "$SERVICE_SRC" "${USER_UNIT_DIR}/${SERVICE_NAME}"

SUDOERS_LINE="${USER_NAME} ALL=(ALL) NOPASSWD: ${BRIDGE}"
echo "$SUDOERS_LINE" >"$SUDOERS"
chmod 440 "$SUDOERS"
visudo -cf "$SUDOERS"

loginctl enable-linger "$USER_NAME" || true

export_user_systemctl systemctl --user daemon-reload
export_user_systemctl systemctl --user enable "$SERVICE_NAME"
export_user_systemctl systemctl --user restart "$SERVICE_NAME" || \
  export_user_systemctl systemctl --user start "$SERVICE_NAME"

echo ""
echo "Installed ${SERVICE_NAME} for ${USER_NAME}."
echo "  status:  systemctl --user status ${SERVICE_NAME}"
echo "  logs:    journalctl --user -u ${SERVICE_NAME} -f"
echo "  linger:  loginctl show-user ${USER_NAME} | grep Linger"
echo ""
echo "The service waits for headset ${BT_MAC:-E4:58:BC:80:90:CD} before starting the bridge."
