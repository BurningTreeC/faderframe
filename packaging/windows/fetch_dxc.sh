#!/usr/bin/env bash
# Put DirectXShaderCompiler's dxcompiler.dll into <dir> (and its licences
# into [licences dir]): wgpu compiles the GPU painter's shaders with it when
# it lies beside the program, and falls back to FXC otherwise -- whose
# shaders crashed Windows' software rasteriser (WARP) on the first frame.
# dxcompiler.dll is open source (LLVM and MIT licences); dxil.dll, under
# Microsoft's own licence, is not needed (the GPU tests pass on WARP with
# dxcompiler.dll alone).
#
#   packaging/windows/fetch_dxc.sh <dir> [licences dir]
set -euo pipefail
dir=${1:?usage: fetch_dxc.sh <dir> [licences dir]}
licences=${2:-}
version=v1.9.2609
zip=dxc_2026_09_29.zip
sha256=ad31b1fc8443175d204f77a611fdb3ef2ec42759bdc2f1167368de24a4a7e7f1
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
curl -sSfL -o "$tmp/$zip" "https://github.com/microsoft/DirectXShaderCompiler/releases/download/$version/$zip"
echo "$sha256  $tmp/$zip" | sha256sum -c --quiet -
# The archive's names use backslashes; unzip turns them into folders.
unzip -q "$tmp/$zip" -d "$tmp/x" 2>/dev/null || true
dll=$(find "$tmp/x" -ipath '*x64*' -iname dxcompiler.dll | head -n1)
[ -n "$dll" ] || { echo "dxcompiler.dll not in $zip" >&2; exit 1; }
mkdir -p "$dir"
cp "$dll" "$dir/"
if [ -n "$licences" ]; then
    mkdir -p "$licences"
    cp "$tmp/x/LICENSE-LLVM.txt" "$licences/DXC-LICENSE-LLVM.txt"
    cp "$tmp/x/LICENCE-MIT.txt" "$licences/DXC-LICENSE-MIT.txt"
fi
echo "$dir/dxcompiler.dll (DXC $version)"
