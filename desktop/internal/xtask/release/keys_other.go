//go:build !unix

package release

import "github.com/ibanks42/rc2fx/desktop/internal/xtask/task"

func createKeys(destination, checkout string) error {
	_, _ = destination, checkout
	return task.Error("mixer-keys currently requires Linux or macOS to enforce private file permissions")
}
