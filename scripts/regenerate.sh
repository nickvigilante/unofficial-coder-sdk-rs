#!/usr/bin/env bash
# Regenerate coder-api-gen from coder/coder at a tag, branch, commit, or local checkout path.
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: scripts/regenerate.sh <coder-ref-or-local-path>" >&2
  exit 2
fi

root="$(cd "$(dirname "$0")/.." && pwd)"
target="$1"
src="$root/.coder-src"

if [[ -d "$target" ]]; then
  src="$(cd "$target" && pwd)"
  ref="local"
  sha="$(git -C "$src" rev-parse --short=10 HEAD)"
else
  rm -rf "$src"
  git init -q "$src"
  git -C "$src" remote add origin https://github.com/coder/coder.git
  git -C "$src" fetch -q --depth 1 origin "$target"
  git -C "$src" checkout -q FETCH_HEAD
  ref="$target"
  sha="$(git -C "$src" rev-parse --short=10 HEAD)"
fi

mkdir -p "$root/spec"
cp "$src/coderd/apidoc/swagger.json" "$root/spec/swagger.json"
(cd "$root/tools/rawfields" && go run . "$src") > "$root/spec/rawfields.json"
npx -y swagger2openapi@7 --patch --outfile "$root/spec/openapi3.json" "$root/spec/swagger.json"
python3 "$root/tools/patch_spec.py" "$root/spec/openapi3.json" "$root/spec/rawfields.json" \
  "$root/spec/openapi.patched.json" "$root/spec/patches.log"
echo "$ref ($sha)" > "$root/spec/coder-ref.txt"
(cd "$root" && cargo xtask generate spec/openapi.patched.json crates/coder-api-gen/src/generated.rs spec/coder-ref.txt)
(cd "$root" && cargo build -p coder-api-gen)
echo "generated from coder/coder $ref ($sha)"
