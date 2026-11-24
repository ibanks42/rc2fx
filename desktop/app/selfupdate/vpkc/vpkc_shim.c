// clang-format off
//go:build velopack
// clang-format on

// The C side of Velopack's callbacks (vpkc.go, callbacks.go). The
// callbacks' C types have const pointers and the Go exports cannot, so these
// pass them on.

#include "Velopack.h"
#include "_cgo_export.h"

static char *feedShim(void *user, const char *name) {
  return rc2fxFeed(user, (char *)name);
}

static void freeFeedShim(void *user, char *feed) { rc2fxFreeFeed(user, feed); }

static bool downloadShim(void *user, const struct vpkc_asset_t *asset,
                         const char *path, size_t id) {
  return rc2fxDownload(user, (vpkc_asset_t *)asset, (char *)path, id);
}

static void progressShim(void *user, size_t progress) {
  rc2fxProgress(user, progress);
}

static void uninstallShim(void *user, const char *version) {
  rc2fxBeforeUninstall(user, (char *)version);
}

vpkc_update_source_t *rc2fxNewSource(void) {
  return vpkc_new_source_custom_callback(feedShim, freeFeedShim, downloadShim,
                                         NULL);
}

void rc2fxSetUninstallHook(void) {
  vpkc_app_set_hook_before_uninstall(uninstallShim);
}

bool rc2fxDownloadUpdates(vpkc_update_manager_t *m, vpkc_update_info_t *u) {
  return vpkc_download_updates(m, u, progressShim, NULL);
}
