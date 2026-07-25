#!/usr/bin/env python3
import json
import pathlib
import sys


def json_pointer_segments(fragment):
    if fragment == "#":
        return []
    if not fragment.startswith("#/"):
        return None
    return [segment.replace("~1", "/").replace("~0", "~") for segment in fragment[2:].split("/")]


def resolve_pointer(document, segments):
    value = document
    for segment in segments:
        if isinstance(value, dict):
            if segment not in value:
                return False
            value = value[segment]
            continue
        if isinstance(value, list):
            if not segment.isdigit():
                return False
            index = int(segment)
            if index >= len(value):
                return False
            value = value[index]
            continue
        return False
    return True


def walk_refs(value, path=()):
    if isinstance(value, dict):
        ref = value.get("$ref")
        if isinstance(ref, str):
            yield path + ("$ref",), ref
        for key, child in value.items():
            yield from walk_refs(child, path + (key,))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from walk_refs(child, path + (str(index),))


def validate_schema(path):
    document = json.loads(path.read_text(encoding="utf-8"))
    errors = []
    for ref_path, ref in walk_refs(document):
        segments = json_pointer_segments(ref)
        if segments is None:
            continue
        if not resolve_pointer(document, segments):
            errors.append(f"{path}:{'/'.join(ref_path)} references unresolved {ref}")
    return errors


def main():
    paths = [pathlib.Path(arg) for arg in sys.argv[1:]]
    if not paths:
        print("usage: validate-schema-refs.py <schema.json> [...]", file=sys.stderr)
        return 2

    errors = []
    for path in paths:
        errors.extend(validate_schema(path))

    if errors:
        print("\n".join(errors), file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
