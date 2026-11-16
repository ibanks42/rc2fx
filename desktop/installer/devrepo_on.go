//go:build rc2fxdev

package installer

import "os"

// devRepository, in development builds only (`-tags rc2fxdev`, Rust's debug
// builds): RC2FX_DEV_MIXER_REPOSITORY=FOLDER reads mixer releases from a
// local folder laid out as published (`FOLDER/metadata/` holds the role
// files, `FOLDER/mixer-vVERSION/` the targets). It is for testing an app and
// mixer built with a throwaway trust root; anything signed for the official
// root verifies the same as it would from GitHub.
func devRepository() (string, bool) {
	return os.LookupEnv("RC2FX_DEV_MIXER_REPOSITORY")
}
