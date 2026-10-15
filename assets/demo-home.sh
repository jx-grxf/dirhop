#!/bin/sh
# Builds the throwaway home folder the demo tapes run in, so recordings never show real files.
# Usage: sh assets/demo-home.sh && vhs assets/demo.tape && vhs assets/setup.tape
set -eu
D=/tmp/dirhop-demo
rm -rf "$D"
mkdir -p "$D" && cd "$D"
repo() { mkdir -p "$1/.git"; echo "ref: refs/heads/${2:-main}" >"$1/.git/HEAD"; }
# A real repo with history, so the preview has a status and a last commit to show.
real_repo() {
  dir=$1 branch=$2 msg=$3
  rm -rf "$dir/.git"
  git -C "$dir" init -q -b "$branch"
  git -C "$dir" add -A
  git -C "$dir" commit -q -m "$msg"
}
repo Projects/weather-app
repo Projects/weather-api feat/forecast
repo Projects/acme-web
repo Projects/dirhop
repo Projects/ml-notebooks
repo Projects/dotfiles
repo Projects/portfolio
repo Projects/clients/acme-ios
mkdir -p Projects/weather-app/Sources/WeatherApp Projects/weather-app/Tests Projects/weather-app/WeatherApp.xcodeproj \
  Projects/weather-api/cmd Projects/weather-api/internal Projects/weather-api/api \
  Projects/acme-web/src/components Projects/acme-web/src/pages Projects/acme-web/public \
  Projects/dirhop/src Projects/dirhop/assets Projects/ml-notebooks/data Projects/ml-notebooks/notebooks \
  Projects/dotfiles/zsh Projects/dotfiles/git Projects/dotfiles/nvim Projects/portfolio/src Projects/portfolio/public \
  Projects/clients/acme-ios/App Documents/Invoices/2026 Documents/Notes Documents/Taxes "Documents/Weather Station Docs" \
  Desktop/Screenshots Downloads Music Pictures/Vacation-2026 Pictures/Wallpapers Movies .config/dirhop
touch Projects/weather-app/Package.swift Projects/weather-app/README.md \
  Projects/weather-api/go.mod Projects/weather-api/go.sum Projects/weather-api/Dockerfile Projects/weather-api/README.md \
  Projects/acme-web/package.json Projects/acme-web/tsconfig.json Projects/acme-web/README.md \
  Projects/dirhop/Cargo.toml Projects/dirhop/README.md Projects/ml-notebooks/pyproject.toml \
  Projects/portfolio/package.json Projects/clients/acme-ios/Package.swift
cat >Projects/weather-app/README.md <<'MD'
# Weather App

A SwiftUI weather app with hourly forecasts, rain radar and a home screen widget.
Data comes from the weather-api service in this workspace.
MD
cat >Projects/weather-api/README.md <<'MD'
# weather-api

Go service that caches forecasts from three providers and serves them as JSON.
MD
cat >Projects/dirhop/README.md <<'MD'
# dirhop

**Jump to any folder in a few keystrokes.** A fast fuzzy folder picker for your terminal.
MD
real_repo Projects/weather-app main "Add hourly forecast chart"
real_repo Projects/weather-api feat/forecast "Cache provider responses for 10 minutes"
real_repo Projects/dirhop main "Show git status in the preview"
echo "draft" >Projects/weather-app/NOTES.md
echo "// wip" >Projects/weather-app/Sources/WeatherApp/Radar.swift

# Some history and a pin, so the empty search shows recent and pinned folders.
now=$(date +%s)
mkdir -p .local/share/dirhop
printf '%s\t%s\t%s\n' \
  12 $((now - 1800)) "$D/Projects/weather-app" \
  7 $((now - 7200)) "$D/Projects/acme-web" \
  4 $((now - 86400 * 2)) "$D/Projects/dirhop" \
  2 $((now - 86400 * 9)) "$D/Documents/Invoices/2026" >.local/share/dirhop/history.tsv
echo "$D/Projects/dotfiles" >.local/share/dirhop/pins.txt

cat >.config/dirhop/config.toml <<'TOML'
command = "hop"
keybinding = "ctrl-g"
priority_roots = ["~/Projects"]
search_roots = ["~"]
editor = "code"
auto_update = true
TOML
