#!/bin/sh
# Installs the root fan helper. Usage: install-helper.sh UID PATH_TO_GAUGE_FAND (run as root)
set -eu
allow_uid=$1
source_binary=$2
case "$allow_uid" in
  ''|*[!0-9]*) echo "bad uid" >&2; exit 2 ;;
esac
# Only the person logged in at the screen, and only a real account, can be given the helper.
console_uid=$(stat -f %u /dev/console)
if [ "$allow_uid" -lt 501 ] || [ "$allow_uid" != "$console_uid" ] || ! id -un "$allow_uid" >/dev/null 2>&1; then
  echo "The helper can only be installed for the account logged in at this Mac." >&2
  exit 2
fi
label=com.gauge.fand
tools=/Library/PrivilegedHelperTools
target=$tools/$label
plist=/Library/LaunchDaemons/$label.plist
install -d -o root -g wheel -m 755 "$tools"
# Copies both files into a root-owned place first, then checks and installs those copies. The
# originals sit in a folder you can write to, so they're never run as root.
staged=$(mktemp "$tools/.gauge-fand.XXXXXX")
staged_uninstall=$(mktemp "$tools/.gauge-uninstall.XXXXXX")
trap 'rm -f "$staged" "$staged_uninstall"' EXIT
install -o root -g wheel -m 755 "$source_binary" "$staged"
install -o root -g wheel -m 755 "$(dirname "$0")/uninstall-helper.sh" "$staged_uninstall"
if ! "$staged" --check; then
  echo "Fan control isn't supported on this Mac, so the helper wasn't installed." >&2
  exit 3
fi
launchctl bootout "system/$label" 2>/dev/null || true
mv -f "$staged" "$target"
# A root-owned copy, so uninstalling never runs a file a normal process could have changed.
mv -f "$staged_uninstall" "$target-uninstall"
cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>$label</string>
  <key>ProgramArguments</key>
  <array>
    <string>$target</string>
    <string>--allow-uid</string>
    <string>$allow_uid</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>StandardErrorPath</key><string>/var/log/gauge-fand.log</string>
</dict>
</plist>
PLIST
chown root:wheel "$plist"
chmod 644 "$plist"
# launchd can take a moment to finish removing the old copy, and refuses a new one until then.
attempts=0
until launchctl bootstrap system "$plist" 2>/dev/null; do
  attempts=$((attempts + 1))
  if [ "$attempts" -ge 10 ]; then
    echo "macOS wouldn't start the fan helper. Restart the Mac, then install it again." >&2
    exit 1
  fi
  sleep 1
done
