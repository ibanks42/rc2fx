// Command uitest runs one offscreen UI test mode selected by RC2FX_UITEST
// against the diagnostic gpuikit library, printing PASS/FAIL and exiting 0 or 1.
// Invoked by uitest_test.go.
package main

import (
	"fmt"
	"os"
	"sort"

	"github.com/ibanks42/rc2fx/desktop/app"
)

func main() {
	mode := os.Getenv("RC2FX_UITEST")
	if mode == "" {
		modes := make([]string, 0, len(app.UITestModes))
		for m := range app.UITestModes {
			modes = append(modes, m)
		}
		sort.Strings(modes)
		fmt.Fprintf(os.Stderr, "set RC2FX_UITEST to one of %v\n", modes)
		os.Exit(2)
	}
	if !app.RunUITest(mode) {
		os.Exit(1)
	}
}
