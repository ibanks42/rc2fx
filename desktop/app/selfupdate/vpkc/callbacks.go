//go:build velopack

package vpkc

// The Go side of Velopack's callbacks. They run on Velopack's threads (the
// source's on the download thread it starts), never on the UI thread, and
// touch no gk state: the app reads their effects through selfupdate's
// Checks and Progress.

/*
#include <stdlib.h>
#include "Velopack.h"
*/
import "C"

import (
	"unsafe"
)

//export rc2fxFeed
func rc2fxFeed(user unsafe.Pointer, releasesName *C.char) *C.char {
	f := currentFeed()
	if f == nil {
		return nil
	}
	feed, err := f.ReleaseFeed(C.GoString(releasesName))
	if err != nil {
		return nil
	}
	return C.CString(string(feed))
}

//export rc2fxFreeFeed
func rc2fxFreeFeed(user unsafe.Pointer, feed *C.char) {
	C.free(unsafe.Pointer(feed))
}

//export rc2fxDownload
func rc2fxDownload(user unsafe.Pointer, asset *C.vpkc_asset_t, localPath *C.char, progressID C.size_t) C.bool {
	f := currentFeed()
	if f == nil || asset == nil || localPath == nil {
		return false
	}
	report := func(percent int16) { C.vpkc_source_report_progress(progressID, C.int16_t(percent)) }
	return C.bool(f.DownloadReleaseEntry(goAsset(asset), C.GoString(localPath), report) == nil)
}

//export rc2fxProgress
func rc2fxProgress(user unsafe.Pointer, progress C.size_t) {
	progressMu.Lock()
	to := progressTo
	progressMu.Unlock()
	if to != nil {
		// The library widens its i16 to size_t; this narrows it back.
		to(int16(uint64(progress)))
	}
}

//export rc2fxBeforeUninstall
func rc2fxBeforeUninstall(user unsafe.Pointer, version *C.char) {
	if uninstallHook != nil {
		uninstallHook()
	}
}
