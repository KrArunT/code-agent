#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/docs/build}"
TITLE="${DOC_TITLE:-AutoFix}"
SOURCE="${DOC_SOURCE:-$ROOT_DIR/README.md}"

mkdir -p "$OUT_DIR"

if ! command -v pandoc >/dev/null 2>&1; then
  echo "pandoc is required to build docs" >&2
  exit 1
fi

pandoc \
  --standalone \
  --toc \
  --metadata title="$TITLE" \
  --from markdown \
  --to html5 \
  --output "$OUT_DIR/$TITLE.html" \
  "$SOURCE"

pandoc \
  --standalone \
  --toc \
  --metadata title="$TITLE" \
  --pdf-engine=xelatex \
  --output "$OUT_DIR/$TITLE.pdf" \
  "$SOURCE"

echo "built $OUT_DIR/$TITLE.html"
echo "built $OUT_DIR/$TITLE.pdf"
