#!/usr/bin/env bash
set -euo pipefail

ARCHIVE_DIR=${1:?usage: release-smoke.sh ARCHIVE_DIR [IMAGE]}
IMAGE=${2:-}
EXECUTE_ARCHIVES=${RELEASE_SMOKE_EXECUTE_ARCHIVES:-1}

shopt -s nullglob
archives=("$ARCHIVE_DIR"/*.tar.gz)
if [ -z "$IMAGE" ]; then
    test "${#archives[@]}" -gt 0
fi

for archive in "${archives[@]:-}"; do
    [ -n "$archive" ] || continue
    workdir=$(mktemp -d)
    trap 'rm -rf "$workdir"' RETURN
    tar -xzf "$archive" -C "$workdir"
    test -x "$workdir/model-gateway"
    test -f "$workdir/gateway.example.toml"
    test -f "$workdir/gateway.core.example.toml"
    test -f "$workdir/gateway.secondary.example.toml"
    if [ "$EXECUTE_ARCHIVES" = 1 ]; then
        "$workdir/model-gateway" --version >/dev/null
        "$workdir/model-gateway" --help >/dev/null
    fi
    python3 - "$workdir" <<'PY'
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
for path in root.rglob("*"):
    if path.is_file() and path.name != "README.md":
        data = path.read_bytes()
        assert b"fixture-secret" not in data
        assert b".model-gateway" not in data
        assert b"OPENROUTER_API_KEY=" not in data
PY
    rm -rf "$workdir"
    trap - RETURN
done

if [ -n "$IMAGE" ]; then
    test "$(docker image inspect "$IMAGE" --format '{{.Config.User}}')" = model-gateway
    test "$(docker image inspect "$IMAGE" --format '{{index .Config.Entrypoint 0}}')" = model-gateway
    docker run --rm --entrypoint model-gateway "$IMAGE" --version >/dev/null
    docker run --rm --entrypoint sh "$IMAGE" -c \
        'test ! -e /app/state/config.toml && test ! -e /run/model-gateway/secrets/fixture && ! grep -R "fixture-secret" /app /run/model-gateway 2>/dev/null'
fi

printf 'Release artifact smoke passed\n'
