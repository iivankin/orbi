#!/bin/sh

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
SOURCE_DIR="$REPO_DIR/schemas"

if [ ! -d "$SOURCE_DIR" ]; then
  printf 'missing schema source directory: %s\n' "$SOURCE_DIR" >&2
  exit 1
fi

if [ "${ORBIT_SCHEMA_DIR:-}" ]; then
  TARGET_DIR=$ORBIT_SCHEMA_DIR
else
  if [ -z "${HOME:-}" ]; then
    printf 'HOME is not set; export ORBIT_SCHEMA_DIR or HOME before running this script\n' >&2
    exit 1
  fi
  TARGET_DIR=$HOME/.orbit/schemas
fi

mkdir -p "$TARGET_DIR"

installed=0
for schema in "$SOURCE_DIR"/*.json; do
  if [ ! -f "$schema" ]; then
    continue
  fi
  cp "$schema" "$TARGET_DIR/$(basename "$schema")"
  printf 'installed %s\n' "$TARGET_DIR/$(basename "$schema")"
  installed=1
done

if [ "$installed" -eq 0 ]; then
  printf 'no schema files found in %s\n' "$SOURCE_DIR" >&2
  exit 1
fi
