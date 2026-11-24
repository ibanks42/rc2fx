package signing

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func workflowFiles(t *testing.T) (env, output string) {
	t.Helper()
	dir := t.TempDir()
	env, output = filepath.Join(dir, "env"), filepath.Join(dir, "output")
	t.Setenv("GITHUB_ENV", env)
	t.Setenv("GITHUB_OUTPUT", output)
	t.Setenv("RUNNER_TEMP", dir)
	return env, output
}

func read(t *testing.T, path string) string {
	t.Helper()
	data, _ := os.ReadFile(path)
	return string(data)
}

func TestWindowsWithoutSettingsStaysUnsigned(t *testing.T) {
	env, output := workflowFiles(t)
	for _, name := range windowsNames {
		t.Setenv(name, "")
	}
	if err := Windows(); err != nil {
		t.Fatal(err)
	}
	if read(t, output) != "enabled=false\n" || read(t, env) != "" {
		t.Errorf("output %q, env %q", read(t, output), read(t, env))
	}
}

func TestWindowsRefusesSomeSettings(t *testing.T) {
	workflowFiles(t)
	for _, name := range windowsNames {
		t.Setenv(name, "")
	}
	t.Setenv("AZURE_TENANT_ID", "tenant")
	if err := Windows(); err == nil {
		t.Error("one setting of five passed")
	}
}

func TestWindowsWritesMetadata(t *testing.T) {
	env, output := workflowFiles(t)
	for _, name := range windowsNames {
		t.Setenv(name, strings.ToLower(name))
	}
	if err := Windows(); err != nil {
		t.Fatal(err)
	}
	metadata := filepath.Join(filepath.Dir(env), "artifact-signing.json")
	if read(t, env) != "RC2FX_ARTIFACT_SIGNING_METADATA="+metadata+"\n" || read(t, output) != "enabled=true\n" {
		t.Errorf("env %q, output %q", read(t, env), read(t, output))
	}
	var document struct {
		Endpoint, CodeSigningAccountName, CertificateProfileName string
		ExcludeCredentials                                       []string
	}
	if err := json.Unmarshal([]byte(read(t, metadata)), &document); err != nil {
		t.Fatal(err)
	}
	if document.Endpoint != "artifact_signing_endpoint" || document.CodeSigningAccountName != "artifact_signing_account" ||
		document.CertificateProfileName != "artifact_signing_profile" || len(document.ExcludeCredentials) != 9 {
		t.Errorf("metadata %+v", document)
	}
}

func TestMacOSWithoutSecretsStaysUnsigned(t *testing.T) {
	env, _ := workflowFiles(t)
	for _, name := range macOSNames {
		t.Setenv(name, "")
	}
	if err := MacOS(); err != nil || read(t, env) != "" {
		t.Errorf("%v, env %q", err, read(t, env))
	}
	t.Setenv("MACOS_CERTIFICATE", "x")
	if err := MacOS(); err == nil {
		t.Error("one secret of seven passed")
	}
}
