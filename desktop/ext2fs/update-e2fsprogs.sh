#!/bin/sh
# Regenerates desktop/ext2fs/e2fsprogs from an e2fsprogs release
# tarball, the way upstream's util/gen-android-files prepares its sources for
# a build without autoconf: config.h is util/android_config.h, ext2_types.h is
# util/android_types.h, and the error tables and CRC table are generated with
# upstream's own awk scripts and generator. Only libext2fs, libcom_err and the
# headers they include are kept.
#
#   sh desktop/ext2fs/update-e2fsprogs.sh e2fsprogs-1.47.4.tar.gz
#
# The sources cgo compiles are the ext2fs_*.c and et_*.c files beside this
# script, one per upstream source (see doc.go); add or remove one there when
# a new release changes lib/ext2fs/Makefile.in's list.
#
# Check the tarball first: its SHA-256 against sha256sums.asc and its
# signature (e2fsprogs-1.47.4.tar.sign, over the uncompressed tar) against
# Theodore Ts'o's key 3AB057B7E78D945C8C5591FBD36F769BC11804F0, both from
# https://mirrors.edge.kernel.org/pub/linux/kernel/people/tytso/e2fsprogs/.
set -eu
tarball=$1
version=1.47.4
sha=da274408bebbfd13a5a2fc3cfc66e3ffff17c48534673aa67f88d49b99123b96
test "$(sha256sum "$tarball" | cut -d' ' -f1)" = "$sha" || { echo "unexpected tarball" >&2; exit 1; }
here=$(cd "$(dirname "$0")" && pwd)
out=$here/e2fsprogs
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
tar -xzf "$tarball" -C "$work"
src=$work/e2fsprogs-$version

rm -rf "$out"
mkdir -p "$out/lib/ext2fs" "$out/lib/et" "$out/lib/e2p" "$out/include"
cp "$src/NOTICE" "$src/RELEASE-NOTES" "$src/version.h" "$out/"
cp "$src"/lib/ext2fs/*.c "$src"/lib/ext2fs/*.h "$out/lib/ext2fs/"
cp "$src"/lib/et/*.c "$src"/lib/et/*.h "$out/lib/et/"
# libext2fs includes e2p.h; it calls nothing from libe2p outside debug code.
cp "$src/lib/e2p/e2p.h" "$out/lib/e2p/"
cp -R "$src/include/mingw" "$src/include/nonunix" "$out/include/"

# The generated files, as gen-android-files makes them.
cp "$src/util/android_config.h" "$out/lib/config.h"
sed 's/#define _LINUX_TYPES_H/#define _EXT2_TYPES_H/' "$src/util/android_types.h" > "$out/lib/ext2fs/ext2_types.h"
sed "s/@E2FSPROGS_VERSION@/$version/" "$src/lib/ext2fs/ext2_err.et.in" > "$work/ext2_err.et"
(cd "$out/lib/ext2fs" &&
  awk -f "$src/lib/et/et_c.awk" outfile=ext2_err.c outfn=ext2_err.c "$work/ext2_err.et" &&
  awk -f "$src/lib/et/et_h.awk" outfile=ext2_err.h outfn=ext2_err.h "$work/ext2_err.et")
cc -o "$work/gen_crc32ctable" "$src/lib/ext2fs/gen_crc32ctable.c"
"$work/gen_crc32ctable" > "$out/lib/ext2fs/crc32c_table.h"
rm -f "$out/lib/ext2fs/gen_crc32ctable.c" "$out/lib/ext2fs/tst_"*.c

# Android's build links libsparse; this one has no sparse images.
sed -i.bak '/ENABLE_LIBSPARSE/d' "$out/lib/config.h" && rm -f "$out/lib/config.h.bak"
echo "e2fsprogs $version, from $(basename "$tarball") (sha256 $sha)" > "$out/SOURCE"
