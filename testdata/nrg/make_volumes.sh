#!/bin/sh
# Builds the file systems that make_nrg.py wraps and packs them as volumes.tar.gz.
# usage: make_volumes.sh WORKDIR   (an empty directory; needs xorriso and macOS hdiutil)
# hdiutil makehybrid only writes a file. Nothing here mounts or attaches an image.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
work=$1
mkdir -p "$work/vol"
python3 "$here/make_nrg.py" trees "$work/trees"
cd "$work"

for i in 01 02 03 04 05 06 07 08 09 10 11 12; do
    xorriso -as mkisofs -quiet --norock -iso-level 1 -no-pad -V "NRG_VOL$i" -o "vol/vol$i.iso" "trees/vol$i"
done

# A data session that follows an audio session: every address is a disc address.
xorriso -as mkisofs -quiet --norock -iso-level 1 -no-pad -V NRG_EXTRA -C 0,11422 -o vol/extra.iso trees/extra

# Two data sessions; the second keeps files of the first. 11400 = 11250 + 150.
X="xorriso -report_about SORRY"
$X -outdev vol/ms1.iso -volid NRG_MS1 -padding 0 -map trees/ms1 /
start=$(( $(wc -c < vol/ms1.iso) / 2048 + 11400 ))
$X -indev vol/ms1.iso -outdev vol/ms2.bin -grow_blindly "$start" -volid NRG_MS2 -padding 0 \
    -map trees/ms2/changed.txt /docs/changed.txt -rm /docs/removed.txt -- \
    -map trees/ms2/new.txt /added/new.txt -map trees/ms2/second.bin /second.bin

hdiutil makehybrid -quiet -hfs -hfs-volume-name "NRG HFS" -o vol/hfs.dmg trees/hfs
hdiutil makehybrid -quiet -hfs -iso -default-volume-name NRG_HYBRID -o vol/hybrid.iso trees/hybrid

python3 "$here/make_nrg.py" pack "$work/vol"
