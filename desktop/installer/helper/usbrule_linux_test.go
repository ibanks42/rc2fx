//go:build linux

package helper

import (
	"strings"
	"testing"
)

func TestUdevadmIsNamedAbsolutelyAndInheritsNoEnvironment(t *testing.T) {
	for _, path := range udevadmPaths {
		if !strings.HasPrefix(path, "/") {
			t.Fatal(path)
		}
	}
	output, err := cleanCommand("/usr/bin/env").Output()
	if err != nil {
		t.Fatal(err)
	}
	if len(output) != 0 {
		t.Fatalf("the child saw an environment: %s", output)
	}
}
