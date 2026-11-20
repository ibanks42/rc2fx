package helper

import "os"

// stagedAppDir tracks a temporary AppDir copy that must outlive the helper process.
type stagedAppDir struct{ path string }

func (s *stagedAppDir) Remove() {
	if s != nil && s.path != "" {
		os.RemoveAll(s.path)
		s.path = ""
	}
}
