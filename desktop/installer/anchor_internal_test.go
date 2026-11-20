package installer

// Keep tests from reading or creating /var/lib/rc2fx/prepared when run as root.
// Tests that need anchor files set Engine.anchorDir explicitly.
func init() {
	anchorDirectory = func(string) (string, error) { return "", nil }
}
