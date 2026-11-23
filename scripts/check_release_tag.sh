#!/bin/sh
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    printf 'usage: %s TAG [MANIFEST]\n' "$0" >&2
    exit 2
fi

tag=$1
manifest=${2:-Cargo.toml}

# Only accept the documented vMAJOR.MINOR.PATCH shape. The tag is passed to
# scripts that run with write access, so keep the accepted surface narrow.
if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
    printf 'invalid release tag %s (expected vMAJOR.MINOR.PATCH)\n' "$tag" >&2
    exit 1
fi

if [ ! -f "$manifest" ]; then
    printf 'manifest not found: %s\n' "$manifest" >&2
    exit 2
fi

workspace_version=$(
    awk '
        /^\[workspace\.package\][[:space:]]*$/ { inside = 1; next }
        /^\[/ { inside = 0 }
        inside && /^[[:space:]]*version[[:space:]]*=/ {
            line = $0
            sub(/^[^=]*=[[:space:]]*"/, "", line)
            sub(/".*/, "", line)
            print line
            exit
        }
    ' "$manifest"
)

if [ -z "$workspace_version" ]; then
    printf 'workspace package version not found in %s\n' "$manifest" >&2
    exit 2
fi

expected_tag="v$workspace_version"
if [ "$tag" != "$expected_tag" ]; then
    printf 'release tag %s does not match workspace version %s (expected %s)\n' \
        "$tag" "$workspace_version" "$expected_tag" >&2
    exit 1
fi

printf 'release tag %s matches workspace version %s\n' "$tag" "$workspace_version"
