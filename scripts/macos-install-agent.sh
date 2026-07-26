#!/bin/sh
set -eu

usage() {
    cat <<EOF
$0: Install CrossDesk.app as a per-user LaunchAgent.

The daemon has to run inside the graphical login session: TCC only grants
Accessibility and Input Monitoring there, and a plain SSH session is not part
of it. A LaunchAgent in ~/Library/LaunchAgents is the way to get that - and
unlike a plist under /tmp it survives a reboot.

usage: $0 [options]

OPTIONS
    --app PATH      bundle to run (default: ./target/CrossDesk.app)
    --label NAME    launchd label (default: $LABEL_DEFAULT)
    --uninstall     remove the agent instead of installing it
    -h, --help      show this help

Logs land in /tmp/<label>.{stdout,stderr}.log.
EOF
}

LABEL_DEFAULT="app.crossdesk.daemon"
LABEL="$LABEL_DEFAULT"
APP="./target/CrossDesk.app"
UNINSTALL=0

while [ $# -gt 0 ]; do
    case "$1" in
        --app) APP="$2"; shift 2 ;;
        --label) LABEL="$2"; shift 2 ;;
        --uninstall) UNINSTALL=1; shift ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

if [ "$(uname -s)" != "Darwin" ]; then
    echo "$0 only runs on macOS" >&2
    exit 1
fi

plist="$HOME/Library/LaunchAgents/$LABEL.plist"
domain="gui/$(id -u)"

if [ "$UNINSTALL" -eq 1 ]; then
    launchctl bootout "$domain/$LABEL" 2>/dev/null || true
    rm -f "$plist"
    echo "removed $LABEL"
    exit 0
fi

APP=$(cd "$(dirname "$APP")" && printf '%s/%s' "$(pwd)" "$(basename "$APP")")
exe="$APP/Contents/MacOS/lan-mouse"
[ -x "$exe" ] || { echo "missing $exe - run scripts/macos-bundle.sh first" >&2; exit 1; }

mkdir -p "$HOME/Library/LaunchAgents"
cat > "$plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>$LABEL</string>
	<key>ProgramArguments</key>
	<array>
		<string>$exe</string>
		<string>daemon</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>KeepAlive</key>
	<dict>
		<key>SuccessfulExit</key>
		<false/>
	</dict>
	<key>ProcessType</key>
	<string>Interactive</string>
	<key>EnvironmentVariables</key>
	<dict>
		<key>LAN_MOUSE_LOG_LEVEL</key>
		<string>info</string>
	</dict>
	<key>StandardOutPath</key>
	<string>/tmp/$LABEL.stdout.log</string>
	<key>StandardErrorPath</key>
	<string>/tmp/$LABEL.stderr.log</string>
</dict>
</plist>
PLIST

launchctl bootout "$domain/$LABEL" 2>/dev/null || true
launchctl bootstrap "$domain" "$plist"
launchctl enable "$domain/$LABEL"

echo "installed $plist"
echo "logs: /tmp/$LABEL.stderr.log"
echo
echo "On first run macOS asks for Accessibility and Input Monitoring."
echo "Grant them to CrossDesk, then restart the agent:"
echo "    launchctl kickstart -k $domain/$LABEL"
