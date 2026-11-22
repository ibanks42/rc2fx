package core

import (
	"math"
	"strings"
	"testing"
)

// Strict JSON parser limits.

func TestExplicitLimitsRetainDuplicateDepthAndTrailingGuards(t *testing.T) {
	ok(t, must2(FromJSONLimited[Value]("[1,2]", 5, 3)))
	fails(t, must2(FromJSONLimited[Value]("[1,2]", 4, 3)))
	fails(t, must2(FromJSONLimited[Value]("[1,2]", 5, 2)))
	fails(t, must2(FromJSONLimited[Value](`{"a":{"x":1,"x":2}}`, 100, 100)))
	fails(t, must2(FromJSONLimited[Value]("[] []", 100, 100)))
	deep := strings.Repeat("[", 65) + "0" + strings.Repeat("]", 65)
	fails(t, must2(FromJSONLimited[Value](deep, 1000, 1000)))
}

func TestHTTPDefaultLimitsRemainUnchanged(t *testing.T) {
	padded := "0" + strings.Repeat(" ", 512*1024)
	fails(t, must2(FromJSON[Value](padded)))
	ok(t, must2(FromJSONLimited[Value](padded, 1024*1024, 200_000)))
	values := "[" + strings.Repeat("0,", 200_000) + "0]"
	fails(t, must2(FromJSON[Value](values)))
	ok(t, must2(FromJSONLimited[Value](values, 512*1024, 400_000)))
}

type numericFixture struct {
	Integer uint64     `json:"integer"`
	Signed  int64      `json:"signed"`
	Float   float64    `json:"float"`
	Values  []*float64 `json:"values"`
}

func TestTypedStorageParserPreservesExactNumericAndStructuralAcceptance(t *testing.T) {
	fixtures := []struct {
		text   string
		accept bool
	}{
		{`{"integer":18446744073709551615,"signed":-9223372036854775808,"float":-0.0,"values":[null,0,1.25,1e-300,1e300]}`, true},
		{`{"integer":9007199254740993,"signed":0,"float":9007199254740993,"values":[]}`, true},
		{`{"integer":1.0,"signed":0,"float":1,"values":[]}`, false},
		{`{"integer":-1,"signed":0,"float":1,"values":[]}`, false},
		{`{"integer":1,"signed":9223372036854775808,"float":1,"values":[]}`, false},
		{`{"integer":1,"signed":0,"float":1e400,"values":[]}`, false},
		{`{"integer":1,"signed":0,"float":1,"values":null}`, false},
		{`{"integer":1,"signed":0,"float":1}`, false},
		{`{"integer":1,"signed":0,"float":1,"values":[],"other":1}`, false},
		{`{"integer":1,"signed":0,"float":1,"values":[],"integer":2}`, false},
	}
	for _, fixture := range fixtures {
		_, err := FromJSON[numericFixture](fixture.text)
		check(t, (err == nil) == fixture.accept, fixture.text, err)
	}
	first := must(FromJSON[numericFixture](fixtures[0].text))
	check(t, first.Integer == math.MaxUint64 && first.Signed == math.MinInt64)
	check(t, first.Float == 0 && math.Signbit(first.Float), "-0.0 keeps its sign")
	check(t, len(first.Values) == 5 && first.Values[0] == nil && *first.Values[1] == 0 && *first.Values[4] == 1e300)
	second := must(FromJSON[numericFixture](fixtures[1].text))
	check(t, second.Integer == 9007199254740993 && second.Float == 9007199254740992)
}
