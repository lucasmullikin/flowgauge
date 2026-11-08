#!/bin/sh
# Removes the root fan helper. launchd stopping it returns the fans to Auto. Run as root.
set -eu
label=com.gauge.fand
launchctl bootout "system/$label" 2>/dev/null || true
rm -f "/Library/LaunchDaemons/$label.plist" "/Library/PrivilegedHelperTools/$label" \
  "/Library/PrivilegedHelperTools/$label-uninstall" /var/run/gauge-fand.sock
