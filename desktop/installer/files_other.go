//go:build !linux

package installer

import "os"

func localOwner(*os.File) error { return nil }

func ownedByInvoker(file *os.File) (*os.File, error) { return file, nil }
