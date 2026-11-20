// Command inspect-firmware audits an official firmware archive against a
// firmware-profiles.json catalog without needing a device or extracted rootfs.
//
//	go -C desktop run ./cmd/inspect-firmware MODEL OS.tar.gz [CATALOG]
package main

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"unicode/utf8"

	"github.com/ibanks42/rc2fx/desktop/firmware"
	"github.com/ibanks42/rc2fx/desktop/installer"
)

// Resolve config/firmware-profiles.json relative to this source file.
func defaultCatalog() (string, error) {
	_, file, _, ok := runtime.Caller(0)
	if !ok || !filepath.IsAbs(file) {
		return "", errors.New("cannot find the checkout; pass the catalog path")
	}
	root := filepath.Dir(filepath.Dir(filepath.Dir(filepath.Dir(file))))
	return filepath.Join(root, "config", "firmware-profiles.json"), nil
}

func run(args []string) error {
	if len(args) != 2 && len(args) != 3 {
		return errors.New("usage: inspect-firmware MODEL OS.tar.gz [CATALOG]")
	}
	model, err := firmware.ParseModel(args[0])
	if err != nil {
		return err
	}
	path := ""
	if len(args) == 3 {
		path = args[2]
	} else if path, err = defaultCatalog(); err != nil {
		return err
	}
	data, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	if !utf8.Valid(data) {
		return fmt.Errorf("%s is not UTF-8", path)
	}
	catalog, err := firmware.ParseCatalog(string(data))
	if err != nil {
		return err
	}
	report, err := installer.InspectArchiveWithCatalog(model, args[1], catalog)
	if err != nil {
		return err
	}
	out, err := json.MarshalIndent(report, "", "  ")
	if err != nil {
		return err
	}
	fmt.Println(string(out))
	return nil
}

func main() {
	if err := run(os.Args[1:]); err != nil {
		fmt.Fprintf(os.Stderr, "inspection refused: %s\n", err)
		os.Exit(1)
	}
}
