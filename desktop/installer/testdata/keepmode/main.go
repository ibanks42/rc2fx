// Command keepmode links the installer the way cmd/rc2fx does, for
// TestPayloadModeSurvivesLinking.
package main

import "github.com/ibanks42/rc2fx/desktop/installer"

func main() {
	installer.KeepPayloadMode()
}
