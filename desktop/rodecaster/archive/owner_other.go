//go:build !linux

package archive

import "os"

// LocalOwner gives a file this process created to the user who started a
// privileged helper; only Linux has such a helper.
func LocalOwner(*os.File) error { return nil }

// OwnedByInvoker returns file if it belongs to the user who started a
// privileged helper; only Linux has such a helper.
func OwnedByInvoker(file *os.File) (*os.File, error) { return file, nil }
