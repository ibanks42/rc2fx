package helper

import "os"

// stagedAppDir is a staged AppDir the elevated helper runs from; it must
// live until the helper exits.
type stagedAppDir struct{ path string }

// Remove deletes the staged copy; nil is a no-op.
func (s *stagedAppDir) Remove() {
	if s != nil && s.path != "" {
		os.RemoveAll(s.path)
		s.path = ""
	}
}
