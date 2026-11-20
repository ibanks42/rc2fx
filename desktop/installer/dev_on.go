//go:build rc2fxdev

package installer

import "os"

// devRepository reads RC2FX_DEV_MIXER_REPOSITORY in rc2fxdev builds to point
// at a local TUF layout (FOLDER/metadata/ and FOLDER/mixer-vVERSION/) for
// testing with a throwaway trust root.
func devRepository() (string, bool) {
	return os.LookupEnv("RC2FX_DEV_MIXER_REPOSITORY")
}

// Allow Request.FixDebug in rc2fxdev builds.
const fixDebugAllowed = true
