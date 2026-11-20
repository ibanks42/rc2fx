//go:build velopack

// Package vpkc wraps Velopack's C library (velopack_libc 1.3.39-g8d2844f,
// matching .github/scripts/setup-vpk.sh) for startup hooks and UpdateManager
// backed by selfupdate.SignedFeed.
//
// Enabled with the `velopack` build tag after desktop/scripts/fetch-velopack.sh
// populates desktop/third_party/velopack. Linux and macOS link statically;
// Windows links velopack_libc.dll (shipped beside rc2fx.exe) via a MinGW
// import library.
//
// -lvelopack_libc is passed via CGO_LDFLAGS by ./xtask app and ./xtask package
// instead of a #cgo directive here, because the static archives include LLVM
// bitcode that Go's #cgo loader rejects. The #cgo directives below supply the
// required system libraries.
package vpkc

/*
#cgo CFLAGS: -I${SRCDIR}/../../../third_party/velopack/include
#cgo linux,amd64 LDFLAGS: -lgcc_s -lutil -lrt -lpthread -lm -ldl
#cgo linux,arm64 LDFLAGS: -lgcc_s -lutil -lrt -lpthread -lm -ldl
#cgo darwin,arm64 LDFLAGS: -framework CoreFoundation -framework Security -framework SystemConfiguration -liconv
#cgo darwin,amd64 LDFLAGS: -framework CoreFoundation -framework Security -framework SystemConfiguration -liconv
#include <stdlib.h>
#include "Velopack.h"

// Defined in vpkc_shim.c.
vpkc_update_source_t *rc2fxNewSource(void);
void rc2fxSetUninstallHook(void);
bool rc2fxDownloadUpdates(vpkc_update_manager_t *m, vpkc_update_info_t *u);
*/
import "C"

import (
	"errors"
	"runtime"
	"sync"
	"unsafe"

	"github.com/ibanks42/rc2fx/desktop/app/selfupdate"
)

var Open selfupdate.Opener = open

var (
	sourceOnce sync.Once
	source     unsafe.Pointer
	// Velopack expects a single custom source for the process lifetime.
	feedMu sync.Mutex
	feed   *selfupdate.SignedFeed
)

func currentFeed() *selfupdate.SignedFeed {
	feedMu.Lock()
	defer feedMu.Unlock()
	return feed
}

func open(f *selfupdate.SignedFeed, locator *selfupdate.LocatorConfig) (selfupdate.Manager, error) {
	feedMu.Lock()
	feed = f
	feedMu.Unlock()
	sourceOnce.Do(func() { source = unsafe.Pointer(C.rc2fxNewSource()) })
	if source == nil {
		return nil, lastError()
	}
	var config *C.vpkc_locator_config_t
	if locator != nil {
		c, free := cLocator(*locator)
		defer free()
		config = c
	}
	var m unsafe.Pointer
	if !C.vpkc_new_update_manager_with_source(source, nil, config, (*unsafe.Pointer)(unsafe.Pointer(&m))) {
		return nil, lastError()
	}
	return &manager{p: m}, nil
}

// manager is a vpkc_update_manager_t, kept for the app's lifetime.
type manager struct{ p unsafe.Pointer }

type update struct {
	p      *C.vpkc_update_info_t
	target selfupdate.Asset
}

func (u *update) TargetFullRelease() selfupdate.Asset { return u.target }

func (u *update) Release() {
	if u.p != nil {
		C.vpkc_free_update_info(u.p)
		u.p = nil
	}
}

func (m *manager) UpdatePendingRestart() (selfupdate.Asset, bool) {
	var asset *C.vpkc_asset_t
	if !C.vpkc_update_pending_restart(m.p, &asset) || asset == nil {
		return selfupdate.Asset{}, false
	}
	defer C.vpkc_free_asset(asset)
	return goAsset(asset), true
}

func (m *manager) CheckForUpdates() (selfupdate.UpdateCheckKind, selfupdate.Update, error) {
	var info *C.vpkc_update_info_t
	switch C.vpkc_check_for_updates(m.p, &info) {
	case C.UPDATE_AVAILABLE:
		u := &update{p: info}
		if info.TargetFullRelease != nil {
			u.target = goAsset(info.TargetFullRelease)
		}
		return selfupdate.UpdateAvailable, u, nil
	case C.NO_UPDATE_AVAILABLE:
		return selfupdate.NoUpdateAvailable, nil, nil
	case C.REMOTE_IS_EMPTY:
		return selfupdate.RemoteIsEmpty, nil, nil
	}
	return 0, nil, lastError()
}

var (
	progressMu sync.Mutex
	progressTo func(int16)
)

func (m *manager) DownloadUpdates(u selfupdate.Update, progress func(int16)) error {
	up, ok := u.(*update)
	if !ok || up.p == nil {
		return errors.New("not an update from this manager")
	}
	progressMu.Lock()
	progressTo = progress
	progressMu.Unlock()
	defer func() {
		progressMu.Lock()
		progressTo = nil
		progressMu.Unlock()
	}()
	if !C.rc2fxDownloadUpdates(m.p, up.p) {
		return lastError()
	}
	return nil
}

func (m *manager) WaitExitThenApplyUpdates(asset selfupdate.Asset, silent, restart bool, args []string) error {
	a, free := cAsset(asset)
	defer free()
	argv := make([]*C.char, len(args))
	for i, arg := range args {
		argv[i] = C.CString(arg)
		defer C.free(unsafe.Pointer(argv[i]))
	}
	var p **C.char
	if len(argv) > 0 {
		cargs := (**C.char)(C.malloc(C.size_t(len(argv)) * C.size_t(unsafe.Sizeof(argv[0]))))
		defer C.free(unsafe.Pointer(cargs))
		copy(unsafe.Slice(cargs, len(argv)), argv)
		p = cargs
	}
	if !C.vpkc_wait_exit_then_apply_updates(m.p, a, C.bool(silent), C.bool(restart), p, C.size_t(len(argv))) {
		return lastError()
	}
	return nil
}

// RunHooks must be called at the start of main. It handles Velopack's
// --veloapp-* lifecycle flags (running the hook and exiting), sets the Windows
// AppUserModelID, cleans up old packages, and disables auto-apply on startup
// so updates only apply on explicit restart.
func RunHooks(beforeUninstall func()) {
	C.vpkc_app_set_auto_apply_on_startup(false)
	if locator, ok := selfupdate.VelopackLocator(); ok {
		c, free := cLocator(locator)
		C.vpkc_app_set_locator(c) // Velopack copies the struct
		free()
	}
	if runtime.GOOS == "windows" && beforeUninstall != nil {
		uninstallHook = beforeUninstall
		C.rc2fxSetUninstallHook()
	}
	C.vpkc_app_run(nil)
}

var uninstallHook func()

func lastError() error {
	buf := make([]byte, 4096)
	for {
		n := int(C.vpkc_get_last_error((*C.char)(unsafe.Pointer(&buf[0])), C.size_t(len(buf))))
		if n <= len(buf) {
			end := max(n-1, 0)
			for i := 0; i < end; i++ {
				if buf[i] == 0 {
					end = i
					break
				}
			}
			return selfupdate.LibraryError(string(buf[:end]))
		}
		buf = make([]byte, n)
	}
}

func goString(p *C.char) string {
	if p == nil {
		return ""
	}
	return C.GoString(p)
}

func goAsset(a *C.vpkc_asset_t) selfupdate.Asset {
	return selfupdate.Asset{
		PackageID:     goString(a.PackageId),
		Version:       goString(a.Version),
		Type:          goString(a.Type),
		FileName:      goString(a.FileName),
		SHA1:          goString(a.SHA1),
		SHA256:        goString(a.SHA256),
		Size:          uint64(a.Size),
		NotesMarkdown: goString(a.NotesMarkdown),
		NotesHTML:     goString(a.NotesHtml),
	}
}

func cAsset(asset selfupdate.Asset) (*C.vpkc_asset_t, func()) {
	a := (*C.vpkc_asset_t)(C.calloc(1, C.size_t(unsafe.Sizeof(C.vpkc_asset_t{}))))
	a.PackageId = C.CString(asset.PackageID)
	a.Version = C.CString(asset.Version)
	a.Type = C.CString(asset.Type)
	a.FileName = C.CString(asset.FileName)
	a.SHA1 = C.CString(asset.SHA1)
	a.SHA256 = C.CString(asset.SHA256)
	a.Size = C.uint64_t(asset.Size)
	a.NotesMarkdown = C.CString(asset.NotesMarkdown)
	a.NotesHtml = C.CString(asset.NotesHTML)
	return a, func() {
		for _, p := range []*C.char{a.PackageId, a.Version, a.Type, a.FileName, a.SHA1, a.SHA256, a.NotesMarkdown, a.NotesHtml} {
			C.free(unsafe.Pointer(p))
		}
		C.free(unsafe.Pointer(a))
	}
}

func cLocator(config selfupdate.LocatorConfig) (*C.vpkc_locator_config_t, func()) {
	c := (*C.vpkc_locator_config_t)(C.calloc(1, C.size_t(unsafe.Sizeof(C.vpkc_locator_config_t{}))))
	c.RootAppDir = C.CString(config.RootAppDir)
	c.UpdateExePath = C.CString(config.UpdateExePath)
	c.PackagesDir = C.CString(config.PackagesDir)
	c.ManifestPath = C.CString(config.ManifestPath)
	c.CurrentBinaryDir = C.CString(config.CurrentBinaryDir)
	c.IsPortable = C.bool(config.IsPortable)
	return c, func() {
		for _, p := range []*C.char{c.RootAppDir, c.UpdateExePath, c.PackagesDir, c.ManifestPath, c.CurrentBinaryDir} {
			C.free(unsafe.Pointer(p))
		}
		C.free(unsafe.Pointer(c))
	}
}
