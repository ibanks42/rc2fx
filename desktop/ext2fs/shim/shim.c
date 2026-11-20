/*
 * The few libext2fs operations RC2FX's mixer repair uses, behind functions
 * that take and return only plain C types, so the Go side never depends on
 * libext2fs's structure layouts. Every filesystem operation is libext2fs's
 * own; nothing here reads or writes the disk itself.
 */
#include "config.h"
#include "ext2fs/ext2_fs.h"
#include "ext2fs/ext2fs.h"
#include "et/com_err.h"

/* Open the filesystem at byte `offset` of `device`. */
errcode_t rc2fx_ext2fs_open(const char *device, unsigned long long offset,
			    int writable, ext2_filsys *fs)
{
	static int tables;
	char options[64];
	int flags = EXT2_FLAG_64BITS | (writable ? EXT2_FLAG_RW : 0);

	/* error_message() names libext2fs's codes once their table is added. */
	if (!tables) {
		initialize_ext2_error_table();
		tables = 1;
	}

	snprintf(options, sizeof(options), "offset=%llu", offset);
	return ext2fs_open2(device, options, flags, 0, 0, default_io_manager, fs);
}

/* The journal still holds changes the kernel has not replayed. */
int rc2fx_ext2fs_needs_recovery(ext2_filsys fs)
{
	return ext2fs_has_feature_journal_needs_recovery(fs->super);
}

errcode_t rc2fx_ext2fs_close(ext2_filsys fs)
{
	return ext2fs_close_free(&fs);
}

/*
 * Look up `path` (relative to the root, without following a final
 * symlink) and report whether it is a regular file and its size.
 */
errcode_t rc2fx_ext2fs_lookup(ext2_filsys fs, const char *path,
			      ext2_ino_t *ino, int *regular,
			      unsigned long long *size)
{
	struct ext2_inode_large inode;
	errcode_t err;

	err = ext2fs_namei(fs, EXT2_ROOT_INO, EXT2_ROOT_INO, path, ino);
	if (err)
		return err;
	err = ext2fs_read_inode_full(fs, *ino, (struct ext2_inode *)&inode,
				     sizeof(inode));
	if (err)
		return err;
	*regular = LINUX_S_ISREG(inode.i_mode);
	*size = EXT2_I_SIZE(&inode);
	return 0;
}

/* Read the whole file into `buf`, which holds exactly `len` bytes. */
errcode_t rc2fx_ext2fs_read(ext2_filsys fs, ext2_ino_t ino,
			    unsigned char *buf, unsigned long long len)
{
	ext2_file_t file;
	errcode_t err, close_err;
	unsigned long long done = 0;
	unsigned int got;

	err = ext2fs_file_open(fs, ino, 0, &file);
	if (err)
		return err;
	if (ext2fs_file_get_size(file) != len) {
		ext2fs_file_close(file);
		return EXT2_ET_FILE_TOO_BIG;
	}
	while (done < len) {
		unsigned int want = len - done > (1u << 20) ? (1u << 20) : (unsigned int)(len - done);
		err = ext2fs_file_read(file, buf + done, want, &got);
		if (err)
			break;
		if (got == 0) {
			err = EXT2_ET_SHORT_READ;
			break;
		}
		done += got;
	}
	close_err = ext2fs_file_close(file);
	return err ? err : close_err;
}

/*
 * Replace the contents of a regular file whose size is exactly `len` with
 * `buf`. The size never changes; libext2fs writes into the file's own
 * blocks, allocating only where a block was never allocated.
 */
errcode_t rc2fx_ext2fs_overwrite(ext2_filsys fs, ext2_ino_t ino,
				 const unsigned char *buf,
				 unsigned long long len)
{
	ext2_file_t file;
	errcode_t err, close_err;
	unsigned long long done = 0;
	unsigned int wrote;

	err = ext2fs_read_bitmaps(fs);
	if (err)
		return err;
	err = ext2fs_file_open(fs, ino, EXT2_FILE_WRITE, &file);
	if (err)
		return err;
	if (ext2fs_file_get_size(file) != len) {
		ext2fs_file_close(file);
		return EXT2_ET_FILE_TOO_BIG;
	}
	while (done < len) {
		unsigned int want = len - done > (1u << 20) ? (1u << 20) : (unsigned int)(len - done);
		err = ext2fs_file_write(file, buf + done, want, &wrote);
		if (err)
			break;
		if (wrote == 0) {
			err = EXT2_ET_SHORT_WRITE;
			break;
		}
		done += wrote;
	}
	close_err = ext2fs_file_close(file);
	return err ? err : close_err;
}

int rc2fx_ext2fs_not_found(errcode_t err)
{
	return err == EXT2_ET_FILE_NOT_FOUND;
}

const char *rc2fx_ext2fs_error(errcode_t err)
{
	return error_message(err);
}
