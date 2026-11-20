//go:build !linux

package archive

import "os"

// LocalOwner is a no-op outside Linux, where there is no privileged helper.
func LocalOwner(*os.File) error { return nil }

// OwnedByInvoker is a no-op outside Linux.
func OwnedByInvoker(file *os.File) (*os.File, error) { return file, nil }
