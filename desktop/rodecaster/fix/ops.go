package fix

import (
	"bytes"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/disk"
	"github.com/ibanks42/rc2fx/desktop/rodecaster/report"
)

// chunk is the largest single read of the grow range.
const chunk = 1 << 20

// now is replaceable in tests so backup names are predictable.
var now = time.Now

// Options are what Fix takes.
type Options struct {
	// Expect, when set, is the SHA-256 (hex) sector 0 must still have.
	Expect string
	// BackupDir is where the copy of the old sector 0 goes; it is created
	// if needed.
	BackupDir string
}

func readFull(d disk.Disk, buf []byte, off int64) error {
	n, err := d.ReadAt(buf, off)
	if n == len(buf) {
		return nil
	}
	if err == nil || err == io.EOF {
		err = io.ErrUnexpectedEOF
	}
	return err
}

func sum(b []byte) string {
	h := sha256.Sum256(b)
	return hex.EncodeToString(h[:])
}

// readFailed is the words for a failed read of the mixer's storage.
func readFailed(what string, err error) string {
	return fmt.Sprintf("%s could not be read (%v). Nothing was changed. Check the USB cable and try again.", what, err)
}

// begin reads sector 0 and fills the parts of the report every command
// shares. When sector 0 is not a known table it looks at the rest of the
// disk to tell damage from another layout. ok is false when the command
// can go no further: rep then holds the failure.
func begin(d disk.Disk, command string) (rep report.Report, sector0 []byte, dec Decision, dm *damage, ok bool) {
	rep = report.Report{Command: command, Sectors: d.Sectors()}
	sector0 = make([]byte, disk.SectorSize)
	if err := readFull(d, sector0, 0); err != nil {
		rep.Code, rep.Error = "read-failed", readFailed("The mixer's storage", err)
		return rep, nil, Decision{}, nil, false
	}
	rep.Sector0 = sum(sector0)
	dec = Decide(sector0, d.Sectors())
	rep.Verdict, rep.Reason, rep.Partitions = dec.Verdict, dec.Reason, dec.Partitions
	if dec.Verdict != report.Unrecognized {
		return rep, sector0, dec, nil, true
	}
	dm, ev, code, reason, err := examine(d, sector0, parse(sector0))
	if err != nil {
		rep.Code, rep.Error = "read-failed", readFailed("The mixer's storage", err)
		return rep, sector0, dec, nil, false
	}
	rep.Evidence = ev
	switch {
	case code != "":
		rep.Code = code
		rep.Reason = dec.Reason + "; " + reason
		rep.Error = fmt.Sprintf("This mixer's partition table is not one rcfix knows (%s), and %s. "+
			"rcfix only rebuilds a table when everything else shows a RØDECaster with the standard layout, so nothing was changed.", dec.Reason, reason)
		return rep, sector0, dec, nil, false
	case dm == nil:
		if reason != dec.Reason {
			rep.Reason = dec.Reason + "; " + reason
		}
		return rep, sector0, dec, nil, true
	}
	rep.Verdict = report.Damaged
	rep.P1Filesystem = dm.p1
	rep.Rebuild = parse(dm.rebuilt)
	return rep, sector0, dec, dm, true
}

// checkEmpty requires every byte partition 1 would grow into to be zero.
func checkEmpty(d disk.Disk, l *Layout) (code, msg string) {
	from, to := growRange(l)
	buf := make([]byte, chunk)
	for s := from; s < to; {
		n := min(uint64(chunk/disk.SectorSize), to-s)
		b := buf[:n*disk.SectorSize]
		if err := readFull(d, b, int64(s*disk.SectorSize)); err != nil {
			return "read-failed", readFailed("The space after partition 1", err)
		}
		if i := firstNonZero(b); i >= 0 {
			bad := s + uint64(i)/disk.SectorSize
			return "not-empty", fmt.Sprintf("The space partition 1 would grow into (sectors %d to %d) is not empty: sector %d holds data. This mixer is not laid out the way this tool expects, so nothing was changed.", from, to-1, bad)
		}
		s += n
	}
	return "", ""
}

func firstNonZero(b []byte) int {
	for i, c := range b {
		if c != 0 {
			return i
		}
	}
	return -1
}

// Check reads the mixer's partition table and says what it found. An
// unknown layout is a successful check with the verdict Unrecognized; a
// damaged one is a successful check with the verdict Damaged.
func Check(d disk.Disk) report.Report {
	rep, _, dec, _, ok := begin(d, "check")
	if !ok {
		return rep
	}
	if dec.Verdict == report.NeedsFix {
		if code, msg := checkEmpty(d, dec.Layout); code != "" {
			rep.Code, rep.Error = code, msg
			return rep
		}
	}
	rep.OK = true
	return rep
}

// refuse is the failure for a table this command does not change.
func refuse(rep report.Report) report.Report {
	if rep.Verdict == report.Damaged {
		rep.Code = "damaged"
		rep.Error = fmt.Sprintf("This mixer's partition table is damaged (%s), so nothing was changed. `rcfix recover` rebuilds it.", rep.Reason)
		return rep
	}
	rep.Code = "unrecognized"
	rep.Error = fmt.Sprintf("This mixer's partition table is not one this tool knows (%s), so nothing was changed.", rep.Reason)
	return rep
}

// expected is false, with the report's failure filled in, when sector 0 is
// not the one --expect named.
func expected(rep *report.Report, expect string) bool {
	if want := strings.ToLower(strings.TrimSpace(expect)); want != "" && want != rep.Sector0 {
		rep.Code = "changed"
		rep.Error = "The mixer's partition table is not the one that was checked before, so nothing was changed. Check it again, then try again."
		return false
	}
	return true
}

// Fix grows partition 1 on a mixer that has RØDE's factory layout, after
// saving a copy of sector 0 in opts.BackupDir.
func Fix(d disk.Disk, opts Options) report.Report {
	rep, old, dec, _, ok := begin(d, "fix")
	if !ok || !expected(&rep, opts.Expect) {
		return rep
	}
	switch rep.Verdict {
	case report.Fixed:
		rep.OK = true
		return rep
	case report.NeedsFix:
	default:
		return refuse(rep)
	}
	if code, msg := checkEmpty(d, dec.Layout); code != "" {
		rep.Code, rep.Error = code, msg
		return rep
	}
	path, err := writeBackup(opts.BackupDir, "rcfix-sector0-", old)
	if err != nil {
		rep.Code = "write-failed"
		rep.Error = fmt.Sprintf("The copy of the current partition table could not be saved (%v), so nothing was changed on the mixer. Choose another folder with --backup-dir and try again.", err)
		return rep
	}
	rep.Backup = path
	hint := fmt.Sprintf(" A copy of the original table is in %s; `rcfix restore %s` writes it back.", path, path)
	after, code, msg := commit(d, old, grown(old, dec.Layout), hint)
	if code != "" {
		rep.Code, rep.Error = code, msg
		return rep
	}
	rep.OK, rep.Changed, rep.After = true, true, parse(after)
	return rep
}

// readBackupFile reads a saved sector 0 and requires it to be a known table
// for a disk of diskSectors.
func readBackupFile(path string, diskSectors uint64) ([]byte, Decision, string) {
	b, err := os.ReadFile(path)
	if err != nil {
		return nil, Decision{}, fmt.Sprintf("The backup file could not be read (%v).", err)
	}
	if len(b) != disk.SectorSize {
		return nil, Decision{}, fmt.Sprintf("%s is %d bytes, but a saved partition table is exactly %d bytes.", path, len(b), disk.SectorSize)
	}
	return b, Decide(b, diskSectors), ""
}

// p1Size reads the size of the filesystem on partition 1.
func p1Size(d disk.Disk, l *Layout) (uint64, error) {
	return ext4Size(sectorReader{d}, uint64(l.Entries[0].Start))
}

// Restore writes back a copy of sector 0 that Fix saved.
func Restore(d disk.Disk, backupPath string) report.Report {
	rep, current, _, _, ok := begin(d, "restore")
	if !ok {
		return rep
	}
	bad := func(msg string) report.Report {
		rep.Code, rep.Error = "bad-backup", msg+" Nothing was changed."
		return rep
	}
	if rep.Verdict == report.Damaged {
		rep.Code = "damaged"
		rep.Error = fmt.Sprintf("This mixer's partition table is damaged (%s), so nothing was changed. `rcfix recover --backup %s` writes the backup over a damaged table.", rep.Reason, backupPath)
		return rep
	}
	backup, bd, problem := readBackupFile(backupPath, d.Sectors())
	if problem != "" {
		return bad(problem)
	}
	if bd.Verdict != report.NeedsFix {
		why := bd.Reason
		if bd.Verdict == report.Fixed {
			why = "it already has the grown partition 1"
		}
		return bad(fmt.Sprintf("%s does not hold RØDE's original partition table: %s.", backupPath, why))
	}
	lo, hi := tableOffset, tableOffset+entrySize
	if !bytes.Equal(current[:lo], backup[:lo]) || !bytes.Equal(current[hi:], backup[hi:]) {
		return bad(fmt.Sprintf("%s does not belong to this mixer, or its partition table has changed since: they differ in more than partition 1.", backupPath))
	}
	if bytes.Equal(current, backup) {
		rep.OK = true
		return rep
	}
	p1, err := p1Size(d, bd.Layout)
	if err != nil {
		rep.Code, rep.Error = "read-failed", readFailed("The start of partition 1", err)
		return rep
	}
	rep.P1Filesystem = p1
	if msg := tooLarge(bd.Layout, p1); msg != "" {
		return bad(msg + " rcfix will not do that.")
	}
	after, code, msg := commit(d, current, backup, " Run `rcfix check` to see the mixer's partition table.")
	if code != "" {
		rep.Code, rep.Error = code, msg
		return rep
	}
	rep.Changed, rep.After = true, parse(after)
	if code, msg := clearGrowRange(d, bd.Layout); code != "" {
		rep.Code, rep.Error = code, msg
		return rep
	}
	rep.OK = true
	return rep
}

// clearGrowRange zeroes the space partition 1 grows into, once the factory
// table is back and nothing uses it. The grown filesystem's data stays
// there otherwise, and Fix (and the carrier script) refuse a space that is
// not empty, so the mixer could never be fixed again. It is checked after.
func clearGrowRange(d disk.Disk, l *Layout) (code, msg string) {
	from, to := growRange(l)
	zero := make([]byte, chunk)
	for s := from; s < to; {
		n := min(uint64(chunk/disk.SectorSize), to-s)
		b := zero[:n*disk.SectorSize]
		if w, err := d.WriteAt(b, int64(s*disk.SectorSize)); err != nil || w != len(b) {
			if err == nil {
				err = io.ErrShortWrite
			}
			return "clear-failed", fmt.Sprintf("The partition table is back as it was, but clearing the space after partition 1 (sectors %d to %d) failed (%v). The fix will not run until it is clear; run restore again.", from, to-1, err)
		}
		s += n
	}
	if err := d.Sync(); err != nil {
		return "clear-failed", fmt.Sprintf("The partition table is back as it was, but clearing the space after partition 1 failed (%v). The fix will not run until it is clear; run restore again.", err)
	}
	if code, msg := checkEmpty(d, l); code != "" {
		return "clear-failed", "The partition table is back as it was, but the space after partition 1 did not read back empty: " + msg + " Run restore again."
	}
	return "", ""
}

// RecoverOptions are what Recover takes.
type RecoverOptions struct {
	// Expect, when set, is the SHA-256 (hex) sector 0 must still have.
	Expect string
	// Backup, when set, is a saved sector 0 to write instead of a rebuilt
	// one.
	Backup string
	// BackupDir is where the copy of the damaged sector 0 goes.
	BackupDir string
}

// Recover puts a damaged sector 0 right: from a saved copy when one is
// given, otherwise rebuilt from what the rest of the disk shows. It never
// touches a known table and never changes the size of an intact entry;
// growing partition 1 is Fix's job.
func Recover(d disk.Disk, opts RecoverOptions) report.Report {
	rep, old, _, dm, ok := begin(d, "recover")
	if !ok || !expected(&rep, opts.Expect) {
		return rep
	}
	switch rep.Verdict {
	case report.NeedsFix, report.Fixed:
		rep.OK = true
		return rep
	case report.Damaged:
	default:
		rep.Code = "unrecognized"
		rep.Error = fmt.Sprintf("This mixer's partition table is not one rcfix knows (%s), but it is not damaged in a way rcfix can repair, so nothing was changed.", rep.Reason)
		return rep
	}
	next := dm.rebuilt
	if opts.Backup != "" {
		b, problem := checkRecoverBackup(dm, old, opts.Backup, d.Sectors())
		if problem != "" {
			rep.Code, rep.Error = "bad-backup", problem+" Nothing was changed."
			return rep
		}
		next, rep.Source = b, opts.Backup
	}
	path, err := writeBackup(opts.BackupDir, "rcfix-damaged-sector0-", old)
	if err != nil {
		rep.Code = "write-failed"
		rep.Error = fmt.Sprintf("The copy of the damaged sector 0 could not be saved (%v), so nothing was changed on the mixer. Choose another folder with --backup-dir and try again.", err)
		return rep
	}
	rep.Backup = path
	hint := fmt.Sprintf(" A copy of the damaged sector 0 is in %s.", path)
	after, code, msg := commit(d, old, next, hint)
	if code != "" {
		rep.Code, rep.Error = code, msg
		return rep
	}
	rep.OK, rep.Changed, rep.After = true, true, parse(after)
	return rep
}

// checkRecoverBackup reads a saved sector 0 for Recover and says what is
// wrong with it, if anything: it must be a known table whose partitions
// start where the evidence on this disk is, keep every intact entry's
// size, and leave room for the filesystem on partition 1.
func checkRecoverBackup(dm *damage, current []byte, path string, diskSectors uint64) ([]byte, string) {
	b, bd, problem := readBackupFile(path, diskSectors)
	if problem != "" {
		return nil, problem
	}
	if bd.Verdict != report.NeedsFix && bd.Verdict != report.Fixed {
		return nil, fmt.Sprintf("%s does not hold a partition table rcfix knows: %s.", path, bd.Reason)
	}
	parts := parse(current)
	for i, p := range bd.Partitions {
		if uint64(p.Start) != dm.evidence[i+1].Sector {
			return nil, fmt.Sprintf("In %s, partition %d starts at sector %d, but on this mixer it starts at sector %d.", path, i+1, p.Start, dm.evidence[i+1].Sector)
		}
		if intact(dm.layout, i, parts[i], diskSectors) && p.Sectors != parts[i].Sectors {
			return nil, fmt.Sprintf("%s would change the size of partition %d, whose entry on the mixer is intact. "+
				"Run rcfix recover without --backup to rebuild the table around it.", path, i+1)
		}
	}
	if dm.p1 > uint64(bd.Partitions[0].Sectors) {
		return nil, tooLarge(dm.layout, dm.p1) + " rcfix will not write " + path + "."
	}
	return b, ""
}

// commit writes next over old at sector 0 and reads it back. On any failure
// it writes old back and says plainly whether that worked; hint is added
// when it did not.
func commit(d disk.Disk, old, next []byte, hint string) (after []byte, code, msg string) {
	fail := func(code, what string) ([]byte, string, string) {
		if err := writeVerified(d, old); err != nil {
			return nil, code, fmt.Sprintf("%s. Putting back what was there before also failed (%v), so the mixer's storage may be left changed. Keep the mixer connected in storage mode.%s", what, err, hint)
		}
		return nil, code, what + ". What was there before was written back and checked: the mixer's storage is back to how it was."
	}
	if n, err := d.WriteAt(next, 0); err != nil || n != len(next) {
		if err == nil {
			err = io.ErrShortWrite
		}
		return fail("write-failed", fmt.Sprintf("Writing the new partition table failed (%v)", err))
	}
	if err := d.Sync(); err != nil {
		return fail("write-failed", fmt.Sprintf("Writing the new partition table failed (%v)", err))
	}
	back := make([]byte, len(next))
	if err := readFull(d, back, 0); err != nil {
		return fail("verify-failed", fmt.Sprintf("The new partition table could not be read back (%v)", err))
	}
	if !bytes.Equal(back, next) {
		return fail("verify-failed", "The partition table read back from the mixer is not what was written")
	}
	return back, "", ""
}

// writeVerified writes b at sector 0, syncs, and reads it back.
func writeVerified(d disk.Disk, b []byte) error {
	n, err := d.WriteAt(b, 0)
	if err != nil {
		return err
	}
	if n != len(b) {
		return io.ErrShortWrite
	}
	if err := d.Sync(); err != nil {
		return err
	}
	back := make([]byte, len(b))
	if err := readFull(d, back, 0); err != nil {
		return err
	}
	if !bytes.Equal(back, b) {
		return errors.New("what was read back differs from what was written")
	}
	return nil
}

func backupName(prefix string, sector0 []byte, t time.Time) string {
	id := binary.LittleEndian.Uint32(sector0[440:444])
	return fmt.Sprintf("%s%08x-%s.bin", prefix, id, t.UTC().Format("20060102T150405Z"))
}

// writeBackup saves sector0 in dir under a new name, durably, and never over
// an existing file.
func writeBackup(dir, prefix string, sector0 []byte) (string, error) {
	if dir == "" {
		return "", errors.New("no backup folder was given")
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return "", err
	}
	path := filepath.Join(dir, backupName(prefix, sector0, now()))
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0o600)
	if err != nil {
		return "", err
	}
	_, err = f.Write(sector0)
	if err == nil {
		err = f.Sync()
	}
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	if err == nil {
		err = syncDir(dir)
	}
	if err != nil {
		os.Remove(path)
		return "", err
	}
	return path, nil
}

// syncDir makes the new file's directory entry durable. Windows cannot
// flush a directory handle; NTFS journals the entry itself.
func syncDir(dir string) error {
	if runtime.GOOS == "windows" {
		return nil
	}
	f, err := os.Open(dir)
	if err != nil {
		return err
	}
	err = f.Sync()
	if cerr := f.Close(); err == nil {
		err = cerr
	}
	return err
}

// SaveSector0 saves a copy of a 512-byte sector 0 in dir under the same kind
// of name a fix gives its backup, so `restore` takes it.
func SaveSector0(dir string, sector0 []byte) (string, error) {
	if len(sector0) != 512 {
		return "", fmt.Errorf("a sector 0 has 512 bytes, not %d", len(sector0))
	}
	return writeBackup(dir, "rcfix-sector0-", sector0)
}
