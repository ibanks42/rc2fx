package uninstall

import (
	"fmt"
	"strings"

	"golang.org/x/sys/windows"
)

// WinUser.h IDYES (missing from x/sys/windows).
const idYes = 6

// OfferCleanup prompts the user to optionally delete RC2FX's local data.
func OfferCleanup() {
	targets := existing()
	if len(targets) == 0 {
		return
	}
	ask := func(text string, flags uint32) int32 {
		t, err1 := windows.UTF16PtrFromString(text)
		title, err2 := windows.UTF16PtrFromString("Uninstall RC2FX")
		if err1 != nil || err2 != nil {
			return 0
		}
		answer, _ := windows.MessageBox(0, t, title, flags)
		return answer
	}
	question := "Also delete RC2FX's data on this PC?\n\n" +
		"This deletes downloaded firmware, the backups of your mixer's RC2FX settings, " +
		"your RC2FX preferences and the SSH key RC2FX made for the mixer's developer " +
		"option.\n\nChoose No to keep them in case you install RC2FX again."
	flags := uint32(windows.MB_YESNO | windows.MB_DEFBUTTON2 | windows.MB_ICONQUESTION | windows.MB_TOPMOST | windows.MB_SETFOREGROUND)
	if ask(question, flags) != idYes {
		return
	}
	var failed []string
	for _, path := range targets {
		if remove(path) != nil {
			failed = append(failed, path)
		}
	}
	if len(failed) > 0 {
		text := fmt.Sprintf("Some RC2FX data could not be deleted. You can delete it yourself:\n\n%s",
			strings.Join(failed, "\n"))
		ask(text, windows.MB_TOPMOST|windows.MB_SETFOREGROUND)
	}
}
