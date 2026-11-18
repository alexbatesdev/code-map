#!/bin/sh
set -eu
project_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
proof_dir=$(mktemp -d "${TMPDIR:-/tmp}/spamton-native-input.XXXXXX")
trap 'rm -rf "$proof_dir"' EXIT HUP INT TERM
xcrun clang -fobjc-arc -Wall -Wextra -Werror -mmacosx-version-min=11.0 \
  "$project_root/native/tests/input.m" -framework AppKit -framework Metal \
  -framework QuartzCore -framework CoreText -framework ImageIO -o "$proof_dir/input-test"
"$proof_dir/input-test"
