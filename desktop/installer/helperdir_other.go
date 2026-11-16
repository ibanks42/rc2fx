//go:build !linux

package installer

func helperDirectory(string) (string, error) { return "", nil }
