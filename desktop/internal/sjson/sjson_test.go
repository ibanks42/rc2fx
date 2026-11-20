package sjson

import (
	"fmt"
	"strings"
	"testing"
)

type strict struct {
	_      struct{} `serde:"deny_unknown_fields"`
	Name   string   `json:"name"`
	Count  uint32   `json:"count"`
	Note   *string  `json:"note"`
	Extra  string   `json:"extra,default"`
	Hidden string   `json:"-"`
}

type lenient struct {
	_     struct{} `serde:"default"`
	Name  string   `json:"name"`
	Items []string `json:"items"`
	Maybe *uint64  `json:"maybe,omitempty"`
}

func TestStructsAreDecodedAsSerdeDoes(t *testing.T) {
	var s strict
	if err := Unmarshal([]byte(`{"name":"a","count":3}`), &s); err != nil || s.Name != "a" || s.Count != 3 || s.Note != nil {
		t.Fatalf("%+v %v", s, err)
	}
	for text, want := range map[string]string{
		`{"name":"a"}`:                                      "missing field `count`",
		`{"name":"a","count":1,"x":1}`:                      "unknown field `x`",
		`{"name":"a","count":1,"name":"b"}`:                 "duplicate field `name`",
		`{"Name":"a","count":1}`:                            "unknown field `Name`",
		`{"name":null,"count":1}`:                           "invalid type: null",
		`{"name":"a","count":-1}`:                           "invalid value",
		`{"name":"a","count":1.0}`:                          "floating point",
		`{"name":"a","count":4294967296}`:                   "invalid value",
		`{"name":"a","count":1} x`:                          "trailing characters",
		`{"name":"\ud800","count":1}`:                       "lone leading surrogate",
		`{"name":"a","count":1,}`:                           "trailing comma",
		strings.Repeat("[", 129) + strings.Repeat("]", 129): "recursion limit",
	} {
		var s strict
		err := Unmarshal([]byte(text), &s)
		if err == nil || !strings.Contains(err.Error(), want) {
			t.Fatalf("%s: %v, want %q", text, err, want)
		}
	}
	var l lenient
	if err := Unmarshal([]byte(`{"other":{"a":[1,2]},"name":"x"}`), &l); err != nil || l.Name != "x" || l.Items != nil {
		t.Fatalf("%+v %v", l, err)
	}
}

func TestOutputMatchesSerdeJSON(t *testing.T) {
	data, err := Marshal(lenient{Name: "<a & \"b\" \x01>"})
	if err != nil {
		t.Fatal(err)
	}
	if string(data) != `{"name":"<a & \"b\"`+" "+`\u0001>","items":[]}` {
		t.Fatalf("%s", data)
	}
	pretty, err := MarshalPretty(map[string]any{"b": []any{}, "a": map[string]any{}, "c": []int{1}})
	if err != nil {
		t.Fatal(err)
	}
	if string(pretty) != "{\n  \"a\": {},\n  \"b\": [],\n  \"c\": [\n    1\n  ]\n}" {
		t.Fatalf("%s", pretty)
	}
}

func TestTaggedEnums(t *testing.T) {
	if v, p, err := Tagged([]byte(`"Probe"`)); err != nil || v != "Probe" || p != nil {
		t.Fatal(v, p, err)
	}
	if v, p, err := Tagged([]byte(`{"Flash":{"serial":"x"}}`)); err != nil || v != "Flash" || string(p) != `{"serial":"x"}` {
		t.Fatal(v, p, err)
	}
	if _, _, err := Tagged([]byte(`{"A":1,"B":2}`)); err == nil {
		t.Fatal("two variants")
	}
}

func TestLastKeyWinsKeepsOneMemberPerKey(t *testing.T) {
	// The shape cargo-about writes: "doctest" twice in each target.
	text := `{"targets":[{"name":"a","doctest":false,"doctest":true}],"x":1,"x":2}`
	if _, err := ParseJSONBytes([]byte(text), DefaultMaxJSONBytes, DefaultMaxJSONNodes); err == nil ||
		!strings.HasPrefix(err.Error(), "Duplicate JSON key: doctest") {
		t.Fatalf("strict parse: %v", err)
	}
	tree, err := ParseJSONBytesLastKeyWins([]byte(text), DefaultMaxJSONBytes, DefaultMaxJSONNodes)
	if err != nil {
		t.Fatal(err)
	}
	if x, _ := tree.Get("x"); x.String() != "2" || len(tree.members) != 2 {
		t.Fatalf("top level: %s", tree.String())
	}
	targets, _ := tree.Get("targets")
	target, _ := targets.Index(0)
	if d, _ := target.Get("doctest"); d.String() != "true" || len(target.members) != 2 {
		t.Fatalf("target: %s", target.String())
	}

	// Past eight keys the lookup switches to a map; the result is the same.
	var b strings.Builder
	b.WriteString("{")
	for i := range 12 {
		fmt.Fprintf(&b, `"k%d":%d,`, i, i)
	}
	b.WriteString(`"k3":"late","k11":"last"}`)
	tree, err = ParseJSONBytesLastKeyWins([]byte(b.String()), DefaultMaxJSONBytes, DefaultMaxJSONNodes)
	if err != nil {
		t.Fatal(err)
	}
	k3, _ := tree.Get("k3")
	k11, _ := tree.Get("k11")
	if len(tree.members) != 12 || k3.String() != `"late"` || k11.String() != `"last"` || tree.members[3].Key != "k3" {
		t.Fatalf("wide object: %s", tree.String())
	}
}
