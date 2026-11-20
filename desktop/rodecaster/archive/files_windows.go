//go:build windows

package archive

import "golang.org/x/sys/windows"

// NoFollow is a no-op on Windows; the cache directory belongs to the current user.
const NoFollow = 0

// ReplaceLocalFile atomically replaces path with temp using write-through.
func ReplaceLocalFile(temp, path string) error {
	from, err := windows.UTF16PtrFromString(temp)
	if err != nil {
		return err
	}
	to, err := windows.UTF16PtrFromString(path)
	if err != nil {
		return err
	}
	return windows.MoveFileEx(from, to, windows.MOVEFILE_REPLACE_EXISTING|windows.MOVEFILE_WRITE_THROUGH)
}
