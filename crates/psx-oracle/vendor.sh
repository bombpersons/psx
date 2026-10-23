#!/bin/sh
# Copies the parts of a Mednafen release that psx-oracle builds into
# mednafen/, from an unpacked tarball:
#
#   curl -LO https://mednafen.github.io/releases/files/mednafen-1.32.1.tar.xz
#   tar xf mednafen-1.32.1.tar.xz
#   crates/psx-oracle/vendor.sh mednafen
#
# Then put back the changes listed in README.md (`git diff` shows them) and
# check `include/config.h` against a fresh `configure` (see README.md).
set -eu
src=$(cd "$1" && pwd)
out=$(cd "$(dirname "$0")" && pwd)/mednafen
rm -rf "$out/src"
mkdir -p "$out/src" "$out/include"
cp "$src/COPYING" "$out/COPYING"

# The frontend-independent core, without the other systems' file loaders.
for f in debug error mempatcher settings endian mednafen git file general memory netplay state state_rewind \
  movie player PSFLoader tests testsexp qtrecord IPSPatcher VirtualFS NativeVFS Stream MemoryStream ExtMemStream \
  FileStream; do
  cp "$src/src/$f.cpp" "$out/src/"
done
cp "$src"/src/*.h "$out/src/"

# Whole directories: their sources, headers and licences.
for d in psx psx/input cdrom cdplay demo cheat_formats compress cputest hash minilzo mthreading net resampler \
  sound string time video quicklz zstd zstd/common zstd/decompress tremor mpcdec trio; do
  mkdir -p "$out/src/$d"
  find "$src/src/$d" -maxdepth 1 -type f \( -name '*.c' -o -name '*.cpp' -o -name '*.h' -o -name '*.inc' \
    -o -name 'COPYING*' -o -name 'LICENSE*' -o -name 'README*' -o -name 'MEDNAFEN-MODIFICATIONS' \) \
    -exec cp {} "$out/src/$d/" \;
done
# Other systems' code, and platform files this build doesn't use.
cd "$out/src"
rm -f cheat_formats/gb.* cheat_formats/snes.* cdrom/scsicd* cdrom/CDAFReader_FLAC.* cputest/ppc_cpu.* \
  mthreading/MThreading_Win32.cpp time/Time_Win32.cpp net/Net_WS2.* sound/okiadpcm_generate.cpp \
  sound/Stereo_Buffer.* sound/Blip_Buffer.*
# Only with --enable-cjk-fonts (12 MB).
rm -f video/font12x13ja.h video/font18x18ko.h
find . -name Makefile.am.inc -delete

# The headers configure links into include/.
mkdir -p "$out/include/trio" "$out/include/minilzo" "$out/include/zstd"
cp "$src"/src/trio/trio.h "$src"/src/trio/triodef.h "$src"/src/trio/trionan.h "$src"/src/trio/triop.h \
  "$src"/src/trio/triostr.h "$out/include/trio/"
cp "$src/include/minilzo_internal/minilzo.h" "$out/include/minilzo/"
cp "$src/src/zstd/zstd.h" "$out/include/zstd/"
# Sources include each other as <mednafen/...>.
ln -sfn ../src "$out/include/mednafen"
