//go:build linux

package archive

// The user who started a privileged helper with sudo or pkexec, who owns
// the files it creates and the cache files it reads.

import (
	"errors"
	"fmt"
	"os"
	"os/user"
	"path/filepath"
	"strconv"
	"syscall"
)

// Account is the gid and home of uid by the password database.
func Account(uid uint32) (uint32, string, bool) {
	u, err := user.LookupId(strconv.FormatUint(uint64(uid), 10))
	if err != nil {
		return 0, "", false
	}
	gid, err := strconv.ParseUint(u.Gid, 10, 32)
	if err != nil || !filepath.IsAbs(u.HomeDir) {
		return 0, "", false
	}
	return uint32(gid), u.HomeDir, true
}

// accountUID is the uid the account name has, by the password database.
func accountUID(name string) (uint32, bool) {
	u, err := user.Lookup(name)
	if err != nil || !filepath.IsAbs(u.HomeDir) {
		return 0, false
	}
	uid, err := strconv.ParseUint(u.Uid, 10, 32)
	if err != nil {
		return 0, false
	}
	return uint32(uid), true
}

// LocalOwner gives files created by the privileged Linux helper to the
// invoking user, especially the SSH key that the desktop client reuses
// later. Never change ownership of an existing file or of staged USB files.
func LocalOwner(file *os.File) error {
	if os.Geteuid() != 0 {
		return nil
	}
	invoker, err := InvokingUser()
	if err != nil || invoker == nil {
		return err
	}
	return file.Chown(int(invoker[0]), int(invoker[1]))
}

// InvokingUser is the user (uid, gid) who started a privileged helper with
// sudo or pkexec.
func InvokingUser() (*[2]uint32, error) {
	return invoker(
		func(name string) (string, bool) { return os.LookupEnv(name) },
		func(uid uint32) (uint32, bool) {
			gid, _, ok := Account(uid)
			return gid, ok
		},
		accountUID,
	)
}

// invoker: pkexec sets PKEXEC_UID itself, so it wins. The SUDO_* variables
// can be inherited by anything, so they count only when the password
// database agrees that SUDO_USER has SUDO_UID.
func invoker(
	variable func(string) (string, bool),
	gidOf func(uint32) (uint32, bool),
	uidOf func(string) (uint32, bool),
) (*[2]uint32, error) {
	parse := func(name string) (*uint32, error) {
		v, ok := variable(name)
		if !ok {
			return nil, nil
		}
		n, err := parseU32(v)
		if err != nil {
			return nil, fmt.Errorf("invalid %s", name)
		}
		return &n, nil
	}
	uid, err := parse("PKEXEC_UID")
	if err != nil {
		return nil, err
	}
	if uid != nil {
		gid, ok := gidOf(*uid)
		if !ok {
			return nil, errors.New("unknown PKEXEC_UID")
		}
		return &[2]uint32{*uid, gid}, nil
	}
	uid, err = parse("SUDO_UID")
	if err != nil {
		return nil, err
	}
	if uid != nil {
		name, ok := variable("SUDO_USER")
		if !ok {
			return nil, errors.New("missing SUDO_USER")
		}
		if found, ok := uidOf(name); !ok || found != *uid {
			return nil, errors.New("SUDO_USER does not match SUDO_UID")
		}
		gid, err := parse("SUDO_GID")
		if err != nil {
			return nil, err
		}
		if gid == nil {
			return nil, errors.New("invalid SUDO_GID")
		}
		return &[2]uint32{*uid, *gid}, nil
	}
	return nil, nil
}

// parseU32 is Rust's u32::from_str: an optional '+', then digits.
func parseU32(text string) (uint32, error) {
	digits := text
	if len(digits) > 0 && digits[0] == '+' {
		digits = digits[1:]
	}
	if digits == "" || digits[0] == '+' || digits[0] == '-' {
		return 0, errors.New("invalid digit")
	}
	n, err := strconv.ParseUint(digits, 10, 32)
	return uint32(n), err
}

// OwnedByInvoker returns file if it belongs to the user who started a
// privileged helper (or to root when there is none), and closes it if not.
func OwnedByInvoker(file *os.File) (*os.File, error) {
	if os.Geteuid() != 0 {
		return file, nil
	}
	invoker, err := InvokingUser()
	if err != nil {
		file.Close()
		return nil, err
	}
	info, err := file.Stat()
	if err != nil {
		file.Close()
		return nil, err
	}
	var invokerUID *uint32
	if invoker != nil {
		invokerUID = &invoker[0]
	}
	stat, ok := info.Sys().(*syscall.Stat_t)
	if !ok || !invokerOwns(stat.Uid, invokerUID) {
		file.Close()
		return nil, &os.PathError{Op: "open", Path: file.Name(), Err: errors.New("a file in the cache folder belongs to another user")}
	}
	return file, nil
}

// invokerOwns: running as root with no invoker, the folder is root's own.
func invokerOwns(owner uint32, invoker *uint32) bool {
	want := uint32(0)
	if invoker != nil {
		want = *invoker
	}
	return owner == want
}
