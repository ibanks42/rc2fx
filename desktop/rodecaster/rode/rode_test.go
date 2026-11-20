package rode

import (
	"testing"

	"github.com/ibanks42/rc2fx/desktop/rodecaster/model"
)

func TestURLGuards(t *testing.T) {
	for _, u := range []string{
		"http://update.rode.com/x",
		"https://rode.com.attacker.test/x",
		"https://user@rode.com/x",
		"https://rode.com:123/x",
		"https://rode.com",
		// Other RØDE hosts are not its update server.
		"https://rode.com/x",
		"https://cdn.rode.com/rcp2/1.7.7/os.tar.gz",
		"https://update.rode.com.attacker.test/x",
		"https://update.rode.com:8443/x",
	} {
		if ValidURL(u) {
			t.Fatalf("%s accepted", u)
		}
	}
	if !ValidURL("https://update.rode.com/manifest.json") {
		t.Fatal("RØDE's manifest refused")
	}
	if _, err := Location(model.ProIi, "https://update.rode.com/duo/1.7.7/os.tar.gz", "os.tar.gz", "1.7.7"); err == nil {
		t.Fatal("another folder accepted")
	}
	got, err := Location(model.ProIi, "update.rode.com/rcp2/1.7.7/os.md5", "os.md5", "1.7.7")
	if err != nil {
		t.Fatal(err)
	}
	if got != "https://update.rode.com/rcp2/1.7.7/os.md5" {
		t.Fatal(got)
	}
	if _, err := Location(model.ProIi, "https://update.rode.com/rcp2/1.7/os.md5", "os.md5", "1.7"); err == nil {
		t.Fatal("two-part version accepted")
	}
}
