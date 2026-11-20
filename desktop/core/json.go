package core

import (
	"errors"

	"github.com/ibanks42/rc2fx/desktop/internal/sjson"
)

// Strict JSON limits shared with rc2fx-core.
const (
	DefaultMaxJSONBytes = sjson.DefaultMaxJSONBytes
	DefaultMaxJSONNodes = sjson.DefaultMaxJSONNodes
	MaxJSONDepth        = sjson.MaxJSONDepth
)

type ValueKind = sjson.Kind

const (
	ValueNull   = sjson.KindNull
	ValueBool   = sjson.KindBool
	ValueNumber = sjson.KindNumber
	ValueString = sjson.KindString
	ValueArray  = sjson.KindArray
	ValueObject = sjson.KindObject
)

type (
	Number                   = sjson.Number
	Member                   = sjson.Member
	Value                    = sjson.Tree
	StrictUnmarshaler        = sjson.StrictUnmarshaler
	UnknownJSONFieldsAllowed = sjson.UnknownJSONFieldsAllowed
)

func NumberFromU64(value uint64) Number          { return sjson.NumberFromU64(value) }
func NumberFromF64(value float64) (Number, bool) { return sjson.NumberFromF64(value) }
func NullValue() Value                           { return sjson.NullTree() }
func BoolValue(value bool) Value                 { return sjson.BoolTree(value) }
func StringValue(value string) Value             { return sjson.StringTree(value) }
func NumberValue(value Number) Value             { return sjson.NumberTree(value) }
func U64Value(value uint64) Value                { return sjson.U64Tree(value) }
func F64Value(value float64) Value               { return sjson.F64Tree(value) }
func ArrayValue(items ...Value) Value            { return sjson.ArrayTree(items...) }
func ObjectValue(members ...Member) Value        { return sjson.ObjectTree(members...) }

func wrapJSONError(err error) error {
	if err == nil {
		return nil
	}
	var se sjson.Error
	if errors.As(err, &se) {
		return CoreError(se)
	}
	return err
}

func ParseJSON(text string) (Value, error) {
	v, err := sjson.ParseJSON(text)
	return v, wrapJSONError(err)
}

func ParseJSONLimited(text string, maximumBytes, maximumNodes int) (Value, error) {
	v, err := sjson.ParseJSONLimited(text, maximumBytes, maximumNodes)
	return v, wrapJSONError(err)
}

func ParseJSONBytes(data []byte, maximumBytes, maximumNodes int) (Value, error) {
	v, err := sjson.ParseJSONBytes(data, maximumBytes, maximumNodes)
	return v, wrapJSONError(err)
}

// ParseJSONBytesLastKeyWins is ParseJSONBytes for tool output that repeats
// keys: a repeated key replaces the earlier value.
func ParseJSONBytesLastKeyWins(data []byte, maximumBytes, maximumNodes int) (Value, error) {
	v, err := sjson.ParseJSONBytesLastKeyWins(data, maximumBytes, maximumNodes)
	return v, wrapJSONError(err)
}

func DecodeStrict(data []byte, out any) error {
	return wrapJSONError(sjson.DecodeStrict(data, out))
}

func DecodeStrictLimited(data []byte, maximumBytes, maximumNodes int, out any) error {
	return wrapJSONError(sjson.DecodeStrictLimited(data, maximumBytes, maximumNodes, out))
}

// FromJSON rejects duplicate keys, oversized input, and deep nesting, then
// decodes strictly into T.
func FromJSON[T any](text string) (T, error) {
	v, err := sjson.FromJSON[T](text)
	return v, wrapJSONError(err)
}

func FromJSONLimited[T any](text string, maximumBytes, maximumNodes int) (T, error) {
	v, err := sjson.FromJSONLimited[T](text, maximumBytes, maximumNodes)
	return v, wrapJSONError(err)
}

func DecodeValue(tree Value, out any) error {
	return wrapJSONError(sjson.DecodeTree(tree, out))
}

func Encode(value any) ([]byte, error) {
	b, err := sjson.Encode(value)
	return b, wrapJSONError(err)
}

func EncodePretty(value any) ([]byte, error) {
	b, err := sjson.EncodePretty(value)
	return b, wrapJSONError(err)
}

func enumUnmarshal[T ~string](target *T, text []byte, variants []T) error {
	for _, variant := range variants {
		if string(variant) == string(text) {
			*target = variant
			return nil
		}
	}
	message := "unknown variant `" + string(text) + "`, expected "
	switch len(variants) {
	case 1:
		message += "`" + string(variants[0]) + "`"
	case 2:
		message += "`" + string(variants[0]) + "` or `" + string(variants[1]) + "`"
	default:
		message += "one of "
		for i, variant := range variants {
			if i > 0 {
				message += ", "
			}
			message += "`" + string(variant) + "`"
		}
	}
	return CoreError(message)
}
