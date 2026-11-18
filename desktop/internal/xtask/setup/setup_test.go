package setup

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"
)

func TestMixerBuildRefusesToReplaceAPromotedBundleLink(t *testing.T) {
	if runtime.GOOS == "windows" {
		t.Skip("unix only")
	}
	root := t.TempDir()
	staging := filepath.Join(root, "mixer")
	bundle := filepath.Join(root, "verified")
	if err := os.Mkdir(bundle, 0o777); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(bundle, "identity"), []byte("preserve"), 0o666); err != nil {
		t.Fatal(err)
	}
	if err := CheckMixerStaging(staging); err != nil {
		t.Fatal(err)
	}
	if err := os.Symlink(bundle, staging); err != nil {
		t.Fatal(err)
	}
	err := CheckMixerStaging(staging)
	if err == nil || !strings.Contains(err.Error(), "./xtask mixer install") {
		t.Fatalf("%v", err)
	}
	if info, err := os.Lstat(staging); err != nil || info.Mode()&os.ModeSymlink == 0 {
		t.Fatal("the link was replaced")
	}
	if data, _ := os.ReadFile(filepath.Join(bundle, "identity")); string(data) != "preserve" {
		t.Fatal("the bundle changed")
	}
}

// The mixer build makes exactly the pinned payload files, with a built ELF
// for exactly the payload's binaries; a file added to the payload but not to
// the build (or the other way round) fails here, without zig.
func TestTheMixerBuildMakesExactlyThePinnedPayloadFiles(t *testing.T) {
	sources := func() map[string]payloadSource {
		return payloadSources("release", "gui", [2]string{"recorder", "snapshot"}, nil, nil)
	}
	if err := checkSources(sources()); err != nil {
		t.Fatal(err)
	}
	missing := sources()
	delete(missing, JackerrSnapshot)
	if err := checkSources(missing); err == nil {
		t.Fatal("a missing source was accepted")
	}
	swapped := sources()
	delete(swapped, "rc2fxd")
	swapped["extra"] = payloadSource{data: []byte("extra")}
	if err := checkSources(swapped); err == nil {
		t.Fatal("a source outside the payload was accepted")
	}
	unbuilt := sources()
	unbuilt["rc2fxd"] = payloadSource{data: []byte("not built")}
	if err := checkSources(unbuilt); err == nil {
		t.Fatal("a binary without a built ELF was accepted")
	}
}
