//go:build !linux

package helper

func prepareLaunch(exe string) (string, *stagedAppDir, error) { return exe, nil, nil }
