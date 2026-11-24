// Package signing prepares code signing in the release workflow: Apple's
// Developer ID certificates in a temporary keychain, and the Azure Artifact
// Signing settings vpk signs Windows packages with. Without the settings it
// warns and the packages stay unsigned.
package signing

import (
	"crypto/rand"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"

	"github.com/ibanks42/rc2fx/tools/xtask/internal/task"
)

// settings returns the values of names, nil when none is set, and an error
// when only some are.
func settings(names []string) (map[string]string, error) {
	values := map[string]string{}
	for _, name := range names {
		if value := os.Getenv(name); value != "" {
			values[name] = value
		}
	}
	if len(values) == 0 {
		return nil, nil
	}
	if len(values) != len(names) {
		fmt.Printf("::error::Set all of %s, or none.\n", strings.Join(names, " "))
		return nil, task.Errorf("set all of %s, or none", strings.Join(names, " "))
	}
	return values, nil
}

// appendTo appends lines to the GitHub Actions workflow file named by
// variable (GITHUB_ENV or GITHUB_OUTPUT).
func appendTo(variable string, lines ...string) error {
	path := os.Getenv(variable)
	if path == "" {
		return task.Errorf("%s is not set; this runs in the release workflow", variable)
	}
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_APPEND|os.O_CREATE, 0o644)
	if err != nil {
		return task.Errorf("%s: %s", variable, task.IOError(err))
	}
	_, err = file.WriteString(strings.Join(lines, "\n") + "\n")
	if closeErr := file.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return task.Errorf("%s: %s", variable, task.IOError(err))
	}
	return nil
}

func runnerTemp() (string, error) {
	dir := os.Getenv("RUNNER_TEMP")
	if dir == "" {
		return "", task.Error("RUNNER_TEMP is not set; this runs in the release workflow")
	}
	return dir, nil
}

var macOSNames = []string{"MACOS_CERTIFICATE", "MACOS_CERTIFICATE_PASSWORD",
	"MACOS_INSTALLER_CERTIFICATE", "MACOS_INSTALLER_CERTIFICATE_PASSWORD",
	"MACOS_NOTARY_KEY", "MACOS_NOTARY_KEY_ID", "MACOS_NOTARY_ISSUER"}

// MacOS is `macos-signing`: it imports the Developer ID certificates and
// notarytool credentials into a temporary keychain and passes their names
// to later steps for `go -C tools/xtask run . package` and vpk.
func MacOS() error {
	values, err := settings(macOSNames)
	if err != nil || values == nil {
		if err == nil {
			fmt.Println("::warning::No Apple signing secrets: users will have to allow this app in Privacy & Security.")
		}
		return err
	}
	temp, err := runnerTemp()
	if err != nil {
		return err
	}
	keychain := filepath.Join(temp, "rc2fx-signing.keychain-db")
	secret := make([]byte, 16)
	if _, err := rand.Read(secret); err != nil {
		return task.Errorf("could not make a keychain password: %s", err)
	}
	password := hex.EncodeToString(secret)
	application := filepath.Join(temp, "rc2fx-application.p12")
	installer := filepath.Join(temp, "rc2fx-installer.p12")
	key := filepath.Join(temp, "rc2fx-notary.p8")
	defer func() {
		for _, file := range []string{application, installer, key} {
			os.Remove(file)
		}
	}()
	for file, name := range map[string]string{application: "MACOS_CERTIFICATE", installer: "MACOS_INSTALLER_CERTIFICATE"} {
		data, err := base64.StdEncoding.DecodeString(strings.Join(strings.Fields(values[name]), ""))
		if err != nil {
			return task.Errorf("%s is not base64: %s", name, err)
		}
		if err := os.WriteFile(file, data, 0o600); err != nil {
			return task.Error(task.IOError(err))
		}
	}
	steps := [][]string{
		{"create-keychain", "-p", password, keychain},
		{"set-keychain-settings", "-lut", "21600", keychain},
		{"unlock-keychain", "-p", password, keychain},
		{"import", application, "-k", keychain, "-P", values["MACOS_CERTIFICATE_PASSWORD"], "-T", "/usr/bin/codesign"},
		{"import", installer, "-k", keychain, "-P", values["MACOS_INSTALLER_CERTIFICATE_PASSWORD"], "-T", "/usr/bin/productbuild"},
	}
	for _, step := range steps {
		if err := task.Run(exec.Command("security", step...)); err != nil {
			return err
		}
	}
	partition := exec.Command("security", "set-key-partition-list", "-S", "apple-tool:,apple:,codesign:,productbuild:",
		"-s", "-k", password, keychain)
	if output, err := partition.Output(); err != nil {
		return task.Errorf("security set-key-partition-list: %s\n%s", err, output)
	}
	if err := task.Run(exec.Command("security", "list-keychains", "-d", "user", "-s", keychain, "login.keychain-db")); err != nil {
		return err
	}

	// codesign and productbuild take full identity names.
	appIdentity, err := identity(keychain, "codesigning", "Developer ID Application")
	if err != nil {
		return err
	}
	if appIdentity == "" {
		fmt.Println("::error::MACOS_CERTIFICATE holds no valid Developer ID Application certificate.")
		return task.Error("MACOS_CERTIFICATE holds no valid Developer ID Application certificate")
	}
	installerIdentity, err := identity(keychain, "basic", "Developer ID Installer")
	if err != nil {
		return err
	}
	if installerIdentity == "" {
		fmt.Println("::error::MACOS_INSTALLER_CERTIFICATE holds no valid Developer ID Installer certificate.")
		return task.Error("MACOS_INSTALLER_CERTIFICATE holds no valid Developer ID Installer certificate")
	}

	const profile = "rc2fx-notary"
	if err := os.WriteFile(key, []byte(values["MACOS_NOTARY_KEY"]), 0o600); err != nil {
		return task.Error(task.IOError(err))
	}
	store := exec.Command("xcrun", "notarytool", "store-credentials", profile, "--key", key,
		"--key-id", values["MACOS_NOTARY_KEY_ID"], "--issuer", values["MACOS_NOTARY_ISSUER"], "--keychain", keychain)
	if output, err := store.CombinedOutput(); err != nil {
		return task.Errorf("notarytool store-credentials: %s\n%s", err, output)
	}
	return appendTo("GITHUB_ENV",
		"RC2FX_MACOS_IDENTITY="+appIdentity,
		"RC2FX_MACOS_INSTALLER_IDENTITY="+installerIdentity,
		"RC2FX_NOTARY_PROFILE="+profile,
		"RC2FX_MACOS_KEYCHAIN="+keychain)
}

// identity is the first valid identity in keychain for policy whose name
// starts with prefix, or "".
func identity(keychain, policy, prefix string) (string, error) {
	output, err := exec.Command("security", "find-identity", "-v", "-p", policy, keychain).Output()
	if err != nil {
		return "", task.Errorf("security find-identity: %s", err)
	}
	match := regexp.MustCompile(`"(` + regexp.QuoteMeta(prefix) + `: [^"]*)"`).FindSubmatch(output)
	if match == nil {
		return "", nil
	}
	return string(match[1]), nil
}

var windowsNames = []string{"AZURE_TENANT_ID", "AZURE_CLIENT_ID",
	"ARTIFACT_SIGNING_ENDPOINT", "ARTIFACT_SIGNING_ACCOUNT", "ARTIFACT_SIGNING_PROFILE"}

// Windows is `windows-signing`: it writes the Azure Artifact Signing
// metadata vpk signs with, from repository variables (not secrets:
// azure/login signs in through GitHub's OIDC token), and sets
// RC2FX_ARTIFACT_SIGNING_METADATA and the step output `enabled`.
func Windows() error {
	values, err := settings(windowsNames)
	if err != nil {
		return err
	}
	if values == nil {
		fmt.Println("::warning::No Azure Artifact Signing settings: Windows users will see an unknown-publisher warning.")
		return appendTo("GITHUB_OUTPUT", "enabled=false")
	}
	temp, err := runnerTemp()
	if err != nil {
		return err
	}
	metadata := filepath.Join(temp, "artifact-signing.json")
	// Sign in only through the Azure CLI session azure/login leaves behind.
	data, err := json.MarshalIndent(map[string]any{
		"Endpoint":               values["ARTIFACT_SIGNING_ENDPOINT"],
		"CodeSigningAccountName": values["ARTIFACT_SIGNING_ACCOUNT"],
		"CertificateProfileName": values["ARTIFACT_SIGNING_PROFILE"],
		"ExcludeCredentials": []string{
			"EnvironmentCredential", "ManagedIdentityCredential", "WorkloadIdentityCredential",
			"SharedTokenCacheCredential", "VisualStudioCredential", "VisualStudioCodeCredential",
			"AzurePowerShellCredential", "AzureDeveloperCliCredential", "InteractiveBrowserCredential",
		},
	}, "", "  ")
	if err != nil {
		return task.Error(err.Error())
	}
	if err := os.WriteFile(metadata, append(data, '\n'), 0o644); err != nil {
		return task.Error(task.IOError(err))
	}
	if err := appendTo("GITHUB_ENV", "RC2FX_ARTIFACT_SIGNING_METADATA="+metadata); err != nil {
		return err
	}
	if err := appendTo("GITHUB_OUTPUT", "enabled=true"); err != nil {
		return err
	}
	fmt.Printf("Artifact Signing metadata: %s\n", metadata)
	return nil
}
