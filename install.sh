#!/bin/sh
# dirhop installer
#   curl -fsSL https://raw.githubusercontent.com/jx-grxf/dirhop/main/install.sh | sh
# Set DIRHOP_INSTALL_DIR to install somewhere other than ~/.local/bin.
set -eu

REPO="jx-grxf/dirhop"
BIN_DIR="${DIRHOP_INSTALL_DIR:-$HOME/.local/bin}"

os=$(uname -s)
arch=$(uname -m)
case "$os-$arch" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-musl ;;
  *)
    echo "dirhop: no prebuilt binary for $os $arch." >&2
    echo "Build it instead: cargo install --git https://github.com/$REPO" >&2
    exit 1
    ;;
esac

asset="dirhop-$target.tar.gz"
base="https://github.com/$REPO/releases/latest/download"
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading dirhop ($target)..."
curl -fsSL --proto '=https' -o "$tmp/$asset" "$base/$asset"
curl -fsSL --proto '=https' -o "$tmp/$asset.sha256" "$base/$asset.sha256"

expected=$(cut -d' ' -f1 <"$tmp/$asset.sha256")
if command -v shasum >/dev/null 2>&1; then
  actual=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
else
  actual=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
fi
if [ "$expected" != "$actual" ]; then
  echo "dirhop: checksum mismatch, aborting." >&2
  exit 1
fi

tar -xzf "$tmp/$asset" -C "$tmp"
mkdir -p "$BIN_DIR"
cp "$tmp/dirhop" "$BIN_DIR/.dirhop.new"
chmod 755 "$BIN_DIR/.dirhop.new"
mv -f "$BIN_DIR/.dirhop.new" "$BIN_DIR/dirhop"
echo "Installed $("$BIN_DIR/dirhop" --version) to $BIN_DIR/dirhop"

case ":$PATH:" in
  *":$BIN_DIR:"*) ;;
  *) echo "Note: $BIN_DIR is not on your PATH. The shell integration works anyway." ;;
esac

if [ -t 1 ] && (: </dev/tty) 2>/dev/null; then
  "$BIN_DIR/dirhop" setup </dev/tty
else
  echo "Run '$BIN_DIR/dirhop setup' to pick your command and shortcut."
fi
