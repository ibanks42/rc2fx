package installer

import (
	"strings"
	"testing"
)

func TestTheSpaceEstimateRoundsToEachRootsBlocks(t *testing.T) {
	var usage rootUsage
	usage.file(1)
	usage.block()
	usage.symlink(10)
	usage.symlink(60)
	if usage.byRoot != [2]uint64{3 * 1024, 3 * 4096} {
		t.Fatalf("%v", usage.byRoot)
	}
}

func TestAnImageThatLeavesTooLittleRoomIsRefused(t *testing.T) {
	var usage rootUsage
	// The 0.2.0 image on 1.7.7: about 375 MiB on the smaller root.
	usage.file(375 << 20)
	must(t, usage.check())
	// Growing past the headroom on the smaller root is refused.
	usage.file(50 << 20)
	err := usage.check()
	if err == nil {
		t.Fatal("accepted")
	}
	for _, want := range []string{"465 MB system partition", "nothing was flashed"} {
		if !strings.Contains(err.Error(), want) {
			t.Fatalf("%v", err)
		}
	}
}

func TestTheManifestLeavesOutWhatTheMixerRewritesAndRC2FXFiles(t *testing.T) {
	for _, name := range []string{"usr/bin/rode-init-update-env.sh", "usr/bin/audio-start.sh"} {
		if !inManifest(name) {
			t.Fatalf("%s is not listed", name)
		}
	}
	for _, name := range volatile {
		if inManifest(name) {
			t.Fatalf("%s is listed", name)
		}
	}
	if inManifest("opt/rc2fx/rc2fxd") || inManifest("odd\nname") {
		t.Fatal("RC2FX's files and odd names are left out")
	}
	if got := line("ab", "etc/hostname"); got != "ab  etc/hostname\n" {
		t.Fatalf("%q", got)
	}
}
