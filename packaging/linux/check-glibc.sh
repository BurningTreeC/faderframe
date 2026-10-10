#!/usr/bin/env bash
# Fail when an ELF in a portable package needs a newer glibc than its stated
# Linux baseline. Usage: check-glibc.sh PACKAGE_DIRECTORY MAXIMUM_VERSION
set -euo pipefail

directory=${1:?usage: check-glibc.sh PACKAGE_DIRECTORY MAXIMUM_VERSION}
maximum=${2:?usage: check-glibc.sh PACKAGE_DIRECTORY MAXIMUM_VERSION}
[ -d "$directory" ] || {
    echo "Package directory not found: $directory" >&2
    exit 2
}

failed=0
checked=0
while IFS= read -r -d '' file; do
    if ! readelf --file-header "$file" >/dev/null 2>&1; then
        continue
    fi
    checked=$((checked + 1))
    while IFS= read -r version; do
        if [ "$version" != "$maximum" ] &&
            [ "$(printf '%s\n%s\n' "$maximum" "$version" | sort -V | tail -n1)" = "$version" ]; then
            echo "$file requires GLIBC_$version (maximum is GLIBC_$maximum)" >&2
            failed=1
        fi
    done < <(
        readelf --version-info "$file" 2>/dev/null |
            sed -n 's/.*GLIBC_\([0-9][0-9.]*\).*/\1/p' |
            sort -Vu
    )
done < <(find "$directory" -type f -print0)

if [ "$checked" -eq 0 ]; then
    echo "No ELF files found in $directory" >&2
    exit 2
fi
if [ "$failed" -ne 0 ]; then
    exit 1
fi
echo "Checked $checked ELF files: all require glibc $maximum or older"
