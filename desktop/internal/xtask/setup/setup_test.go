package setup

import (
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// Rust's #[ignore]: requires zig; builds only the two recorder payloads,
// never audio/app. Run with RC2FX_XTASK_ZIG_TESTS=1.
func TestRecorderBuilderProducesBothCheckedArmPayloads(t *testing.T) {
	if os.Getenv("RC2FX_XTASK_ZIG_TESTS") != "1" {
		t.Skip("requires zig; set RC2FX_XTASK_ZIG_TESTS=1")
	}
	root := task.Root()
	outputs, err := BuildJackRecorder(root, filepath.Join(root, "target", "recorder-builder-test"))
	if err != nil {
		t.Fatal(err)
	}
	if filepath.Base(outputs[0]) != JackerrModule || filepath.Base(outputs[1]) != JackerrSnapshot {
		t.Fatalf("%v", outputs)
	}
	for _, path := range outputs {
		if info, err := os.Stat(path); err != nil || !info.Mode().IsRegular() {
			t.Fatalf("%s is not a file", path)
		}
	}
}

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
