// Command uitest runs the app's offscreen UI tests: one mode per run, chosen
// by RC2FX_UITEST, under the diag library. It prints
// `PASS name` or `FAIL name: why` and exits 0 or 1. uitest_test.go builds
// and runs it.
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
