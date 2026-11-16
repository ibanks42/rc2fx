package licenses

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/ibanks42/rc2fx/desktop/core"
	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

func TestCopyrightHTMLKeepsUnicodeEntitiesAndVerbatimTerms(t *testing.T) {
	text, err := HTMLText("<body><pre>Copyright &#34;A&#34; &amp; B\nTerms</pre></body>")
	if err != nil || text != "\nCopyright \"A\" & B\nTerms\n" {
		t.Fatalf("%q %v", text, err)
	}
	if _, err := HTMLText("<body>&unrecognized;</body>"); err == nil {
		t.Fatal("an unknown entity was accepted")
	}
}

func TestBothFormatsPreserveNoticeTextAndEscapeHTML(t *testing.T) {
	sections := []Section{{"Owner <&>", "Copyright A\nTerms <&>\n\"quoted\""}}
	doc := Render(sections)
	value, err := core.ParseJSONBytes(doc.JSON, 1<<20, 10_000)
	if err != nil {
		t.Fatal(err)
	}
	items, _ := value.Get("sections")
	first, _ := items.Index(0)
	text, _ := first.Get("text")
	if s, _ := text.AsString(); s != sections[0].Text {
		t.Fatalf("%q", s)
	}
	html := string(doc.HTML)
	if !strings.Contains(html, "Owner &lt;&amp;&gt;") || !strings.Contains(html, "Copyright A\nTerms &lt;&amp;&gt;") {
		t.Fatal(html)
	}
	if VerifyEmbedded([]byte("old executable"), doc.JSON) == nil {
		t.Fatal("an executable without the document passed")
	}
	if err := VerifyEmbedded(doc.JSON, doc.JSON); err != nil {
		t.Fatal(err)
	}
}

func report(t *testing.T, text string) core.Value {
	t.Helper()
	value, err := core.ParseJSON(text)
	if err != nil {
		t.Fatal(err)
	}
	return value
}

func TestDifferentCopyrightsAreNotLostWhenLicenseNamesMatch(t *testing.T) {
	groups := Groups{}
	for _, pair := range [][2]string{{"A", "one"}, {"B", "two"}, {"A", "three"}} {
		r := report(t, `{"licenses":[{"name":"MIT","text":"Copyright `+pair[0]+`","used_by":[{"crate":{"name":"`+pair[1]+`","version":"1.0"}}]}]}`)
		if err := MergeReport(groups, r); err != nil {
			t.Fatal(err)
		}
	}
	if len(groups["MIT"]) != 2 || len(groups["MIT"]["Copyright A"]) != 2 {
		t.Fatalf("%v", groups)
	}
	if MergeReport(groups, report(t, `{"licenses":[]}`)) == nil {
		t.Fatal("an empty report was accepted")
	}
}

func TestTheVelopackReportMatchesThePinnedVpkAndMerges(t *testing.T) {
	path := filepath.Join(task.Root(), "licenses", "velopack-"+VPKVersion+".json")
	data, err := os.ReadFile(path)
	if err != nil {
		t.Fatal(err)
	}
	r, err := core.ParseJSONBytes(data, maxReportBytes, maxReportNodes)
	if err != nil {
		t.Fatal(err)
	}
	if v, _ := r.Get("velopack"); !isString(v, VPKVersion) {
		t.Fatalf("report is for %s", v.String())
	}
	groups := Groups{}
	if err := MergeReport(groups, r); err != nil {
		t.Fatal(err)
	}
	var users []string
	for _, texts := range groups {
		for _, set := range texts {
			for user := range set {
				users = append(users, user)
			}
		}
	}
	for _, name := range []string{"velopack_bins ", "zstd-sys ", "webpki-roots "} {
		found := false
		for _, user := range users {
			found = found || strings.HasPrefix(user, name)
		}
		if !found {
			t.Errorf("%s has no notice", name)
		}
	}
}

func TestGpuikitNoticesMergeIdenticalTextsAndKeepTheListing(t *testing.T) {
	dir := t.TempDir()
	write := func(path, text string) {
		full := filepath.Join(dir, filepath.FromSlash(path))
		if err := os.MkdirAll(filepath.Dir(full), 0o777); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(full, []byte(text), 0o666); err != nil {
			t.Fatal(err)
		}
	}
	write("THIRD_PARTY.md", "# Third-party software in libgpuikit\n")
	write("licences/texts/a-1.0/LICENSE-MIT", "MIT terms")
	write("licences/texts/b-2.0/LICENSE", "MIT terms")
	write("licences/texts/c-3.0/LICENSE-APACHE", "Apache terms")
	sections, err := Gpuikit(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(sections) != 2 || !strings.Contains(sections[0].Text, "libgpuikit") {
		t.Fatalf("%+v", sections)
	}
	text := sections[1].Text
	if !strings.Contains(text, "Used by: a-1.0 (LICENSE-MIT), b-2.0 (LICENSE)\n\nMIT terms") ||
		!strings.Contains(text, "Used by: c-3.0 (LICENSE-APACHE)\n\nApache terms") {
		t.Fatal(text)
	}
	if _, err := Gpuikit(filepath.Join(dir, "missing")); err == nil {
		t.Fatal("a missing download was accepted")
	}
}
