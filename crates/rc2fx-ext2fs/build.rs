//! Builds libext2fs and libcom_err from the vendored e2fsprogs sources, with
//! the configuration header of upstream's Android build and the source list
//! of lib/ext2fs/Makefile.in (Android.bp's in 1.47.4 lacks getenv.c,
//! orphan.c, inode_io.c, bb_compat.c and write_bb_file.c). Left out:
//! sparse_io.c (libsparse), undo_io.c and tdb.c, and test_io.c, none of which
//! the repair uses.
use std::path::Path;

const EXT2FS: &[&str] = &[
    "ext2_err.c",
    "alloc.c",
    "alloc_sb.c",
    "alloc_stats.c",
    "alloc_tables.c",
    "atexit.c",
    "badblocks.c",
    "bb_compat.c",
    "bb_inode.c",
    "bitmaps.c",
    "bitops.c",
    "blkmap64_ba.c",
    "blkmap64_rb.c",
    "blknum.c",
    "block.c",
    "bmap.c",
    "check_desc.c",
    "crc16.c",
    "crc32c.c",
    "csum.c",
    "closefs.c",
    "dblist.c",
    "dblist_dir.c",
    "digest_encode.c",
    "dirblock.c",
    "dirhash.c",
    "dir_iterate.c",
    "dupfs.c",
    "expanddir.c",
    "ext_attr.c",
    "extent.c",
    "fallocate.c",
    "fileio.c",
    "finddev.c",
    "flushb.c",
    "freefs.c",
    "gen_bitmap.c",
    "gen_bitmap64.c",
    "get_num_dirs.c",
    "get_pathname.c",
    "getsize.c",
    "getsectsize.c",
    "getenv.c",
    "hashmap.c",
    "i_block.c",
    "icount.c",
    "imager.c",
    "ind_block.c",
    "initialize.c",
    "inline.c",
    "inline_data.c",
    "inode_io.c",
    "inode.c",
    "io_manager.c",
    "ismounted.c",
    "link.c",
    "llseek.c",
    "lookup.c",
    "mmp.c",
    "mkdir.c",
    "mkjournal.c",
    "namei.c",
    "native.c",
    "newdir.c",
    "nls_utf8.c",
    "openfs.c",
    "orphan.c",
    "progress.c",
    "punch.c",
    "qcow2.c",
    "rbtree.c",
    "read_bb.c",
    "read_bb_file.c",
    "res_gdt.c",
    "rw_bitmaps.c",
    "sha256.c",
    "sha512.c",
    "swapfs.c",
    "symlink.c",
    "unlink.c",
    "valid_blk.c",
    "version.c",
    "write_bb_file.c",
];

const ET: &[&str] = &[
    "error_message.c",
    "et_name.c",
    "init_et.c",
    "com_err.c",
    "com_right.c",
];

fn main() {
    // Linux only until libext2fs is built and tested for macOS and Windows.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("linux") {
        return;
    }
    let root = Path::new("e2fsprogs");
    let windows = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows");
    let mut build = cc::Build::new();
    build
        .include(root.join("lib"))
        .include(root.join("lib/ext2fs"))
        .include(root.join("lib/et"))
        // Upstream's code, built as upstream builds it; its warnings are
        // theirs to fix.
        .warnings(false)
        .flag_if_supported("-fno-strict-aliasing");
    if windows {
        build.include(root.join("include/mingw"));
    }
    let io = if windows { "windows_io.c" } else { "unix_io.c" };
    for file in EXT2FS.iter().chain([&io]) {
        build.file(root.join("lib/ext2fs").join(file));
    }
    for file in ET {
        build.file(root.join("lib/et").join(file));
    }
    build.file("src/shim.c");
    build.compile("ext2fs");
    println!("cargo:rerun-if-changed=e2fsprogs");
    println!("cargo:rerun-if-changed=src/shim.c");
}
