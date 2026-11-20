package core

import (
	"math"
	"os"
	"path/filepath"
	"testing"
)

// repoFile reads a file relative to the repository root.
func repoFile(t *testing.T, parts ...string) []byte {
	t.Helper()
	data, err := os.ReadFile(filepath.Join(append([]string{"..", ".."}, parts...)...))
	if err != nil {
		t.Fatal(err)
	}
	return data
}

// easyFixture is fixtures/easyeffects/<name>.
func easyFixture(t *testing.T, name string) string {
	t.Helper()
	return string(repoFile(t, "fixtures", "easyeffects", name))
}

// parse reads a test input as a Value, with no size or node limit.
func parse(t *testing.T, text string) Value {
	t.Helper()
	value, err := ParseJSONLimited(text, math.MaxInt, math.MaxInt)
	if err != nil {
		t.Fatalf("parse %q: %v", text, err)
	}
	return value
}

// toValue is serde_json::to_value, failing the test on error.
func toValue(t *testing.T, v any) Value {
	t.Helper()
	data, err := Encode(v)
	if err != nil {
		t.Fatal(err)
	}
	value, err := ParseJSONBytes(data, math.MaxInt, math.MaxInt)
	if err != nil {
		t.Fatal(err)
	}
	return value
}

// encode is Encode as a string, failing the test on error.
func encode(t *testing.T, v any) string {
	t.Helper()
	data, err := Encode(v)
	if err != nil {
		t.Fatal(err)
	}
	return string(data)
}

// must is unwrap(): a failure panics, which fails the test with a trace.
func must[T any](value T, err error) T {
	if err != nil {
		panic(err)
	}
	return value
}

func ok(t *testing.T, err error) {
	t.Helper()
	if err != nil {
		t.Fatal(err)
	}
}

func fails(t *testing.T, err error, context ...any) {
	t.Helper()
	if err == nil {
		t.Fatalf("expected an error %v", context)
	}
}

func check(t *testing.T, condition bool, context ...any) {
	t.Helper()
	if !condition {
		t.Fatalf("check failed %v", context)
	}
}

// imported is an EasyEffects import that must be ready.
func imported(t *testing.T, text string, stereo bool) EQSettings {
	t.Helper()
	result := must(ImportEasyEffects(text, ImportOptions{Stereo: stereo}))
	if result.Kind != ImportResultReady {
		t.Fatalf("unexpected import result: %+v", result)
	}
	return result.EQ
}

func receive(t *testing.T, session *Session, state DeviceState) RemoteUpdate {
	t.Helper()
	return must(session.Receive(state))
}

func approx(a, b, tolerance float64) bool { return math.Abs(a-b) < tolerance }
