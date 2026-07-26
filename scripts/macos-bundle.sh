#!/bin/sh
set -eu

usage() {
    cat <<EOF
$0: Build, bundle and sign CrossDesk.app for macOS.

macOS grants TCC permissions (Accessibility, Input Monitoring) to a code
signing identity, not to a path. A bare binary is signed ad-hoc, so its
identity is its code hash and every rebuild invalidates the grant - the app
then silently loses its permissions and cannot be re-authorized by hand,
because a process that never *asks* for a permission is never registered
with TCC in a usable way.

This script produces a bundle signed with a stable self-signed identity, so
the designated requirement stays

    identifier "app.crossdesk.CrossDesk" and certificate leaf = H"<fixed>"

across rebuilds and the permissions granted once keep working.

usage: $0 [options]

OPTIONS
    --no-build          use the existing target/release/lan-mouse
    --identity NAME     signing identity (default: $IDENTITY_DEFAULT)
    --app PATH          bundle to write (default: ./target/CrossDesk.app)
    -h, --help          show this help

The signing identity is created on first use as a self-signed code signing
certificate in a dedicated keychain, so it never touches the login keychain
and needs no password from the user. Delete ~/.crossdesk-signing and the
keychain to start over - note that this changes the identity and therefore
requires re-granting the permissions.
EOF
}

IDENTITY_DEFAULT="CrossDesk Local Signing"
IDENTITY="$IDENTITY_DEFAULT"
APP="./target/CrossDesk.app"
BUILD=1
KEYCHAIN="$HOME/Library/Keychains/crossdesk-signing.keychain-db"
KEYCHAIN_PASSWORD="crossdesk"
SIGNING_DIR="$HOME/.crossdesk-signing"
BUNDLE_ID="app.crossdesk.CrossDesk"

while [ $# -gt 0 ]; do
    case "$1" in
        --no-build) BUILD=0; shift ;;
        --identity) IDENTITY="$2"; shift 2 ;;
        --app) APP="$2"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 1 ;;
    esac
done

if [ "$(uname -s)" != "Darwin" ]; then
    echo "$0 only runs on macOS" >&2
    exit 1
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
[ -n "$version" ] || version="0.0.0"

if [ "$BUILD" -eq 1 ]; then
    echo "==> building lan-mouse (release)"
    cargo build --release --bin lan-mouse
fi

binary="./target/release/lan-mouse"
[ -f "$binary" ] || { echo "missing $binary" >&2; exit 1; }

# --- signing identity -------------------------------------------------------
# `security find-identity -v` only lists *trusted* identities, and marking a
# certificate as trusted needs an interactive authorization. codesign is happy
# with an untrusted self-signed identity, and TCC only cares that the leaf
# certificate stays the same, so trust is not needed here.
if ! security find-certificate -c "$IDENTITY" "$KEYCHAIN" >/dev/null 2>&1; then
    echo "==> creating signing identity \"$IDENTITY\""
    mkdir -p "$SIGNING_DIR"
    if [ ! -f "$SIGNING_DIR/cert.pem" ]; then
        openssl req -x509 -newkey rsa:2048 -nodes -days 3650 \
            -keyout "$SIGNING_DIR/key.pem" -out "$SIGNING_DIR/cert.pem" \
            -subj "/CN=$IDENTITY" \
            -addext "basicConstraints=critical,CA:false" \
            -addext "keyUsage=critical,digitalSignature" \
            -addext "extendedKeyUsage=critical,codeSigning"
        # Security.framework cannot read OpenSSL 3's default PKCS#12 encryption
        openssl pkcs12 -export -name "$IDENTITY" \
            -inkey "$SIGNING_DIR/key.pem" -in "$SIGNING_DIR/cert.pem" \
            -out "$SIGNING_DIR/identity.p12" -passout "pass:$KEYCHAIN_PASSWORD" \
            -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1
    fi
    if [ ! -f "$KEYCHAIN" ]; then
        security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
        security set-keychain-settings "$KEYCHAIN"  # no auto-lock
        # keep the keychain searchable without dropping the existing ones
        security list-keychains -d user -s \
            "$HOME/Library/Keychains/login.keychain-db" "$KEYCHAIN"
    fi
    security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
    security import "$SIGNING_DIR/identity.p12" -k "$KEYCHAIN" \
        -P "$KEYCHAIN_PASSWORD" -T /usr/bin/codesign -A
    security set-key-partition-list -S apple-tool:,apple:,codesign: \
        -s -k "$KEYCHAIN_PASSWORD" "$KEYCHAIN" >/dev/null
fi

# --- bundle -----------------------------------------------------------------
echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$binary" "$APP/Contents/MacOS/lan-mouse"
[ -f ./target/icon.icns ] && cp ./target/icon.icns "$APP/Contents/Resources/"
[ -f ./target/menubar-template.png ] && cp ./target/menubar-template.png "$APP/Contents/Resources/"

{
    cat <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleExecutable</key><string>lan-mouse</string>
	<key>CFBundleIdentifier</key><string>$BUNDLE_ID</string>
	<key>CFBundleName</key><string>CrossDesk</string>
	<key>CFBundleDisplayName</key><string>CrossDesk</string>
	<key>CFBundlePackageType</key><string>APPL</string>
	<key>CFBundleShortVersionString</key><string>$version</string>
	<key>CFBundleVersion</key><string>$version</string>
	<key>LSMinimumSystemVersion</key><string>11.0</string>
PLIST
    [ -f ./target/icon.icns ] && printf '\t<key>CFBundleIconFile</key><string>icon</string>\n'
    # single source of truth for the TCC usage strings and LSUIElement
    cat ./build-aux/macos-lsui-element.plist
    printf '</dict>\n</plist>\n'
} > "$APP/Contents/Info.plist"

# --- sign -------------------------------------------------------------------
echo "==> signing"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
codesign --force --sign "$IDENTITY" --identifier "$BUNDLE_ID" \
    --keychain "$KEYCHAIN" "$APP"
codesign --verify --strict "$APP"

echo "==> designated requirement (stable across rebuilds):"
codesign -d -r- "$APP" 2>&1 | sed -n 's/^designated => /    /p'
echo "==> done: $APP"
