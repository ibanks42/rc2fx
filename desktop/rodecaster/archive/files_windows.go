//go:build windows

package archive

import "golang.org/x/sys/windows"

// NoFollow has no Windows equivalent here; reparse points are not followed
// by the checks that matter (the cache folder is the user's own).
const NoFollow = 0

// ReplaceLocalFile renames temp over path, writing through.
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
