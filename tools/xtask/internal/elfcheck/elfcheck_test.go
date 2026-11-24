package elfcheck

import "testing"

func TestAcceptsOnlyFirmwareSymbolVersions(t *testing.T) {
	for _, version := range []string{"GLIBC_2.17", "GLIBC_2.35", "GCC_3.0", "GCC_4.2.0", "GCC_11.0"} {
		if !Supported(version) {
			t.Errorf("%s", version)
		}
	}
	for _, version := range []string{"GLIBC_2.36", "GLIBC_2.39", "GLIBC_PRIVATE", "GCC_12.0", "CXXABI_1.3"} {
		if Supported(version) {
			t.Errorf("%s", version)
		}
	}
}
