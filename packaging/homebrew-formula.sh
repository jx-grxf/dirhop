#!/bin/sh
# Prints the Homebrew formula for a release.
#   sh packaging/homebrew-formula.sh 0.2.0 dist/ > Formula/dirhop.rb
# dist/ holds the release archives' .sha256 files.
set -eu
version=$1
dist=$2
base="https://github.com/jx-grxf/dirhop/releases/download/v$version"
sum() { cut -d' ' -f1 <"$dist/dirhop-$1.tar.gz.sha256"; }

cat <<RUBY
class Dirhop < Formula
  desc "Fast fuzzy folder picker for your terminal"
  homepage "https://github.com/jx-grxf/dirhop"
  version "$version"
  license "MIT"

  on_macos do
    on_arm do
      url "$base/dirhop-aarch64-apple-darwin.tar.gz"
      sha256 "$(sum aarch64-apple-darwin)"
    end
    on_intel do
      url "$base/dirhop-x86_64-apple-darwin.tar.gz"
      sha256 "$(sum x86_64-apple-darwin)"
    end
  end

  on_linux do
    on_arm do
      url "$base/dirhop-aarch64-unknown-linux-musl.tar.gz"
      sha256 "$(sum aarch64-unknown-linux-musl)"
    end
    on_intel do
      url "$base/dirhop-x86_64-unknown-linux-musl.tar.gz"
      sha256 "$(sum x86_64-unknown-linux-musl)"
    end
  end

  def install
    bin.install "dirhop"
  end

  def caveats
    <<~EOS
      Run \`dirhop setup\` once to pick a command name and a shortcut.
    EOS
  end

  test do
    assert_match version.to_s, shell_output("#{bin}/dirhop --version")
  end
end
RUBY
