package core

import (
	"math"
	"strings"
	"testing"
)

// Extra strict-decoder and serde-compatible encoder tests beyond the shared fixtures.

func TestFloatsAreWrittenAsSerdeJSONWritesThem(t *testing.T) {
	for _, c := range []struct {
		value float64
		want  string
	}{
		{0, "0.0"},
		{math.Copysign(0, -1), "-0.0"},
		{1, "1.0"},
		{80, "80.0"},
		{-18, "-18.0"},
		{0.1, "0.1"},
		{0.7, "0.7"},
		{1e15, "1000000000000000.0"},
		{1e16, "1e+16"},
		{1.5e16, "1.5e+16"},
		{123456789012345.6, "123456789012345.6"},
		{1e-5, "0.00001"},
		{1.25e-5, "0.0000125"},
		{1e-6, "1e-6"},
		{1.5e-7, "1.5e-7"},
		{5e-324, "5e-324"},
		{math.MaxFloat64, "1.7976931348623157e+308"},
		{math.NaN(), "null"},
		{math.Inf(1), "null"},
	} {
		got := string(must(Encode(c.value)))
		check(t, got == c.want, c.value, got, c.want)
	}
}

func TestStringsEscapeOnlyWhatSerdeJSONEscapes(t *testing.T) {
	got := encode(t, "\"\\/\b\f\n\r\t\x00\x1f\x7f<>&\u2028\u00e9")
	check(t, got == `"\"\\/\b\f\n\r\t\u0000\u001f`+"\x7f<>&\u2028\u00e9"+`"`, got)
	check(t, encode(t, "a\xffb") == "\"a\ufffdb\"", "invalid UTF-8 becomes U+FFFD")
}

func TestPrettyOutputMatchesSerdeJSON(t *testing.T) {
	type inner struct {
		List  []int          `json:"list"`
		Empty []int          `json:"empty"`
		Map   map[string]int `json:"map"`
	}
	type outer struct {
		Name  string  `json:"name"`
		Inner inner   `json:"inner"`
		None  *uint32 `json:"none,omitempty"`
		Null  *uint32 `json:"null"`
	}
	got := string(must(EncodePretty(outer{Name: "x", Inner: inner{List: []int{1, 2}, Map: map[string]int{"b": 2, "a": 1}}})))
	want := "{\n  \"name\": \"x\",\n  \"inner\": {\n    \"list\": [\n      1,\n      2\n    ],\n    \"empty\": [],\n" +
		"    \"map\": {\n      \"a\": 1,\n      \"b\": 2\n    }\n  },\n  \"null\": null\n}"
	check(t, got == want, got)
}

func TestStrictDecoderRules(t *testing.T) {
	type rules struct {
		Required  uint8    `json:"required"`
		Optional  *uint8   `json:"optional"`
		Defaulted uint8    `json:"defaulted,default"`
		NonNull   *[]uint8 `json:"nonNull,omitempty,nonnull"`
	}
	value := must(FromJSON[rules](`{"required":1}`))
	check(t, value.Required == 1 && value.Optional == nil && value.Defaulted == 0 && value.NonNull == nil)
	value = must(FromJSON[rules](`{"required":255,"optional":null,"defaulted":2,"nonNull":[]}`))
	check(t, value.Required == 255 && value.Optional == nil && value.Defaulted == 2 && value.NonNull != nil)
	for _, bad := range []string{
		`{}`,
		`{"required":256}`,
		`{"required":-1}`,
		`{"required":1.0}`,
		`{"required":"1"}`,
		`{"required":1,"defaulted":null}`,
		`{"required":1,"nonNull":null}`,
		`{"required":1,"extra":true}`,
		`[1]`,
	} {
		fails(t, must2(FromJSON[rules](bad)), bad)
	}
	_, err := FromJSON[rules](`{"required":1,"extra":true}`)
	check(t, err != nil && strings.HasPrefix(err.Error(), "unknown field `extra`"), err)
	_, err = FromJSON[TargetKind](`"speaker"`)
	check(t, err != nil && err.Error() == "unknown variant `speaker`, expected one of `mic`, `playback`, `output`", err)
	_, err = FromJSON[rules](`{"required":1,"required":2}`)
	check(t, err != nil && strings.HasPrefix(err.Error(), "Duplicate JSON key: required"), err)
}

func TestNumbersKeepTheirSerdeJSONKind(t *testing.T) {
	for _, c := range []struct {
		text, want string
	}{
		{"0", "0"},
		{"-0", "-0.0"},
		{"1.0", "1.0"},
		{"1e2", "100.0"},
		{"-5", "-5"},
		{"18446744073709551615", "18446744073709551615"},
		{"18446744073709551616", "1.8446744073709552e+19"},
		{"-9223372036854775808", "-9223372036854775808"},
		{"-9223372036854775809", "-9.223372036854776e+18"},
		{"123456789012345678901", "1.2345678901234567e+20"},
		{"1.2345678901234567e-300", "1.234567890123457e-300"},
		{"1e-400", "0.0"},
		{"-1e-400", "-0.0"},
	} {
		got := must(ParseJSON(c.text)).String()
		check(t, got == c.want, c.text, got, c.want)
	}
	for _, bad := range []string{"1e400", "-1e400", "1e99999999999", "01", "-", "1.", ".5", "1e", "+1", "0x10"} {
		fails(t, must2(ParseJSON(bad)), bad)
	}
}

func TestValuePointersAndOrder(t *testing.T) {
	value := parse(t, `{"b":[{"a~/b":1}],"a":2}`)
	*value.Pointer("/b/0/a~0~1b") = U64Value(3)
	check(t, value.String() == `{"b":[{"a~/b":3}],"a":2}`, value.String())
	check(t, value.Pointer("/b/01") == nil && value.Pointer("/c") == nil && value.Pointer("b") == nil)
	value.Set("c", NullValue())
	value.Remove("b")
	check(t, value.String() == `{"a":2,"c":null}`, value.String())
	check(t, value.Equal(parse(t, `{"c":null,"a":2}`)))
	check(t, !U64Value(1).Equal(F64Value(1)))
}
