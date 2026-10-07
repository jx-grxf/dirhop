#!/bin/sh
# Builds the throwaway home folder the demo tapes run in, so recordings never show real files.
# Usage: sh assets/demo-home.sh && vhs assets/demo.tape && vhs assets/setup.tape
set -eu
D=/tmp/dirhop-demo
rm -rf "$D"
mkdir -p "$D" && cd "$D"
repo() { mkdir -p "$1/.git"; echo "ref: refs/heads/${2:-main}" >"$1/.git/HEAD"; }
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
cat >.config/dirhop/config.toml <<'TOML'
command = "hop"
keybinding = "ctrl-g"
priority_roots = ["~/Projects"]
search_roots = ["~"]
editor = "code"
auto_update = true
TOML
