// Package ext2fs implements offline ext4 filesystem access in pure Go so the
// desktop app can repair a RØDECaster mixer's ext4 root partitions without
// mounting them or requiring cgo.
//
// It exposes only the minimal surface needed for repair: open a filesystem at a
// byte offset, check whether journal recovery is pending, look up a path, read
// a regular file, and overwrite a regular file in place with the same byte
// length.
package ext2fs

import "fmt"

// Error describes an ext4 parsing, validation, or I/O error.
type Error struct {
	Code    int64
	Message string
}

func (e *Error) Error() string { return e.Message }

func invalid(message string) *Error { return &Error{Message: message} }

func errf(format string, args ...any) *Error {
	return &Error{Message: fmt.Sprintf(format, args...)}
}

// Entry describes a filesystem entry returned by Lookup.
type Entry struct {
	Inode   uint32
	Regular bool
	Size    uint64
}
