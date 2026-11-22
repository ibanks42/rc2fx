// Package schema generates shared Rust and Go wire type definitions from the
// Go-syntax schema files under schema/.
package schema

import (
	"bytes"
	"fmt"
	"go/ast"
	"go/format"
	"go/parser"
	"go/token"
	"os"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
	"unicode"

	"github.com/ibanks42/rc2fx/desktop/internal/xtask/task"
)

// Run regenerates all schema targets (or verifies they are up to date when
// check is true).
func Run(check bool) error {
	root := task.Root()
	targets := []struct {
		srcRel string
		goRel  string
		goPkg  string
		rsRel  string
	}{
		{
			srcRel: "schema/core.go",
			goRel:  "desktop/core/schema_gen.go",
			goPkg:  "core",
			rsRel:  "crates/rc2fx-core/src/schema_gen.rs",
		},
		{
			srcRel: "schema/payload.go",
			goRel:  "desktop/installer/embed/lists_gen.go",
			goPkg:  "embed",
			rsRel:  "crates/rc2fx-device/src/mixer_update/payload_lists_gen.rs",
		},
	}

	var stale []string
	for _, tgt := range targets {
		srcPath := filepath.Join(root, filepath.FromSlash(tgt.srcRel))
		goPath := filepath.Join(root, filepath.FromSlash(tgt.goRel))
		rsPath := filepath.Join(root, filepath.FromSlash(tgt.rsRel))

		srcBytes, err := os.ReadFile(srcPath)
		if err != nil {
			return fmt.Errorf("read %s: %w", srcPath, err)
		}
		if check {
			formatted, err := format.Source(srcBytes)
			if err != nil {
				return fmt.Errorf("format %s: %w", srcPath, err)
			}
			if !bytes.Equal(srcBytes, formatted) {
				return task.Errorf("%s is not gofmt-formatted; run `gofmt -w %s`", tgt.srcRel, tgt.srcRel)
			}
		}

		doc, err := ParseFile(tgt.srcRel, srcBytes)
		if err != nil {
			return err
		}
		goOut, err := EmitGo(doc, tgt.goPkg, tgt.srcRel)
		if err != nil {
			return fmt.Errorf("emit Go for %s: %w", tgt.srcRel, err)
		}
		rsOut, err := EmitRust(doc, tgt.srcRel)
		if err != nil {
			return fmt.Errorf("emit Rust for %s: %w", tgt.srcRel, err)
		}

		if check {
			for _, out := range []struct {
				rel  string
				path string
				want []byte
			}{
				{tgt.goRel, goPath, goOut},
				{tgt.rsRel, rsPath, rsOut},
			} {
				got, err := os.ReadFile(out.path)
				if err != nil || !bytes.Equal(got, out.want) {
					stale = append(stale, out.rel)
				}
			}
			continue
		}

		if err := os.WriteFile(goPath, goOut, 0o644); err != nil {
			return err
		}
		if err := os.WriteFile(rsPath, rsOut, 0o644); err != nil {
			return err
		}
	}

	if check && len(stale) > 0 {
		return task.Errorf("generated schema files are out of date (%s); run `./xtask schema`", strings.Join(stale, ", "))
	}
	return nil
}

// Document is the parsed representation of a schema/*.go file.
type Document struct {
	Consts      []ConstDef
	StringLists []StringListDef
	Enums       []EnumDef
	Structs     []StructDef
	Order       []DeclRef
}

type StringListDef struct {
	Name   string
	Values []string
	Docs   []string
}

type DeclKind int

const (
	DeclEnum DeclKind = iota
	DeclStruct
)

type DeclRef struct {
	Kind  DeclKind
	Index int
}

type ConstDef struct {
	Name   string
	GoType string // "" for untyped
	Value  string // literal source text
	Kind   token.Token
	Docs   []string
}

type EnumDef struct {
	Name     string
	Docs     []string
	All      bool
	IntEnum  bool
	Fallback string // variant suffix, e.g. "Other"
	Variants []EnumVariant
}

type EnumVariant struct {
	GoName   string // full Go const name, e.g. DiagnosticsKindUSBGadget
	Suffix   string // without enum prefix, e.g. USBGadget
	WireName string // JSON wire string, e.g. "usbGadget"
	Docs     []string
}

type StructDef struct {
	Name         string
	Docs         []string
	AllowUnknown bool
	Default      bool
	Copy         bool
	EmptyCheck   bool
	Fields       []FieldDef
}

type FieldDef struct {
	Name      string
	Type      TypeRef
	JSONName  string
	OmitEmpty bool
	Default   bool
	NonNull   bool
	Docs      []string
}

type TypeRef struct {
	Pointer  bool
	Slice    bool
	ArrayLen int // >0 for fixed array [N]T
	Elem     string
	// For *[]T (e.g. RightBands *[]Band), Pointer=true and Slice=true.
}

// ParseFile parses a Go-syntax schema file into a Document.
func ParseFile(filename string, src []byte) (*Document, error) {
	fset := token.NewFileSet()
	file, err := parser.ParseFile(fset, filename, src, parser.ParseComments)
	if err != nil {
		return nil, fmt.Errorf("parse %s: %w", filename, err)
	}

	doc := &Document{}
	enumIndex := map[string]int{}

	for _, decl := range file.Decls {
		gen, ok := decl.(*ast.GenDecl)
		if !ok {
			return nil, fmt.Errorf("%s: only const and type declarations are allowed in schema files", filename)
		}
		switch gen.Tok {
		case token.TYPE:
			for _, spec := range gen.Specs {
				ts := spec.(*ast.TypeSpec)
				docs, directives := parseCommentGroup(commentFor(ts.Doc, gen.Doc))
				switch t := ts.Type.(type) {
				case *ast.Ident:
					if t.Name != "string" {
						return nil, fmt.Errorf("%s: enum %s must have underlying type string, got %s", filename, ts.Name.Name, t.Name)
					}
					ed := EnumDef{
						Name:     ts.Name.Name,
						Docs:     docs,
						All:      directives.has("all"),
						IntEnum:  directives.has("int_enum"),
						Fallback: directives.get("fallback"),
					}
					enumIndex[ed.Name] = len(doc.Enums)
					doc.Order = append(doc.Order, DeclRef{Kind: DeclEnum, Index: len(doc.Enums)})
					doc.Enums = append(doc.Enums, ed)
				case *ast.StructType:
					sd := StructDef{
						Name:         ts.Name.Name,
						Docs:         docs,
						AllowUnknown: directives.has("allow_unknown"),
						Default:      directives.has("default"),
						Copy:         directives.has("copy"),
						EmptyCheck:   directives.has("empty_check"),
					}
					for _, f := range t.Fields.List {
						if len(f.Names) != 1 {
							return nil, fmt.Errorf("%s: struct %s fields must be named individually", filename, sd.Name)
						}
						if f.Tag == nil {
							return nil, fmt.Errorf("%s: %s.%s is missing a json tag", filename, sd.Name, f.Names[0].Name)
						}
						tagVal, err := strconv.Unquote(f.Tag.Value)
						if err != nil {
							return nil, fmt.Errorf("%s: %s.%s invalid tag: %w", filename, sd.Name, f.Names[0].Name, err)
						}
						jsonTag := reflect.StructTag(tagVal).Get("json")
						if jsonTag == "" {
							return nil, fmt.Errorf("%s: %s.%s missing json tag value", filename, sd.Name, f.Names[0].Name)
						}
						parts := strings.Split(jsonTag, ",")
						tr, err := parseTypeExpr(f.Type)
						if err != nil {
							return nil, fmt.Errorf("%s: %s.%s: %w", filename, sd.Name, f.Names[0].Name, err)
						}
						fDocs, _ := parseCommentGroup(f.Doc)
						if len(fDocs) == 0 && f.Comment != nil {
							fDocs, _ = parseCommentGroup(f.Comment)
						}
						fd := FieldDef{
							Name:     f.Names[0].Name,
							Type:     tr,
							JSONName: parts[0],
							Docs:     fDocs,
						}
						for _, opt := range parts[1:] {
							switch opt {
							case "omitempty":
								fd.OmitEmpty = true
							case "default":
								fd.Default = true
							case "nonnull":
								fd.NonNull = true
							default:
								return nil, fmt.Errorf("%s: %s.%s: unsupported json tag option %q", filename, sd.Name, fd.Name, opt)
							}
						}
						sd.Fields = append(sd.Fields, fd)
					}
					doc.Order = append(doc.Order, DeclRef{Kind: DeclStruct, Index: len(doc.Structs)})
					doc.Structs = append(doc.Structs, sd)
				default:
					return nil, fmt.Errorf("%s: unsupported type %s", filename, ts.Name.Name)
				}
			}
		case token.CONST:
			for _, spec := range gen.Specs {
				vs := spec.(*ast.ValueSpec)
				if len(vs.Names) != 1 || len(vs.Values) != 1 {
					return nil, fmt.Errorf("%s: const specs must have one name and one value", filename)
				}
				name := vs.Names[0].Name
				lit, ok := vs.Values[0].(*ast.BasicLit)
				if !ok {
					return nil, fmt.Errorf("%s: const %s must be a basic literal", filename, name)
				}
				goType := ""
				if vs.Type != nil {
					ident, ok := vs.Type.(*ast.Ident)
					if !ok {
						return nil, fmt.Errorf("%s: const %s has unsupported type", filename, name)
					}
					goType = ident.Name
				}
				docs, _ := parseCommentGroup(commentFor(vs.Doc, vs.Comment))
				if idx, isEnum := enumIndex[goType]; isEnum {
					wire, err := strconv.Unquote(lit.Value)
					if err != nil {
						return nil, fmt.Errorf("%s: enum variant %s must be a string literal", filename, name)
					}
					suffix, ok := strings.CutPrefix(name, goType)
					if !ok || suffix == "" {
						return nil, fmt.Errorf("%s: enum variant %s must start with %s", filename, name, goType)
					}
					doc.Enums[idx].Variants = append(doc.Enums[idx].Variants, EnumVariant{
						GoName:   name,
						Suffix:   suffix,
						WireName: wire,
						Docs:     docs,
					})
					continue
				}
				doc.Consts = append(doc.Consts, ConstDef{
					Name:   name,
					GoType: goType,
					Value:  lit.Value,
					Kind:   lit.Kind,
					Docs:   docs,
				})
			}
		case token.VAR:
			for _, spec := range gen.Specs {
				vs := spec.(*ast.ValueSpec)
				if len(vs.Names) != 1 || len(vs.Values) != 1 {
					return nil, fmt.Errorf("%s: var specs must have one name and one value", filename)
				}
				name := vs.Names[0].Name
				comp, ok := vs.Values[0].(*ast.CompositeLit)
				if !ok {
					return nil, fmt.Errorf("%s: var %s must be a string array literal", filename, name)
				}
				arrType, ok := comp.Type.(*ast.ArrayType)
				if !ok {
					return nil, fmt.Errorf("%s: var %s must be an array of strings", filename, name)
				}
				elt, ok := arrType.Elt.(*ast.Ident)
				if !ok || elt.Name != "string" {
					return nil, fmt.Errorf("%s: var %s must be an array of strings", filename, name)
				}
				var vals []string
				for _, e := range comp.Elts {
					lit, ok := e.(*ast.BasicLit)
					if !ok || lit.Kind != token.STRING {
						return nil, fmt.Errorf("%s: var %s elements must be string literals", filename, name)
					}
					s, err := strconv.Unquote(lit.Value)
					if err != nil {
						return nil, fmt.Errorf("%s: var %s: %w", filename, name, err)
					}
					vals = append(vals, s)
				}
				docs, _ := parseCommentGroup(commentFor(vs.Doc, gen.Doc))
				doc.StringLists = append(doc.StringLists, StringListDef{
					Name:   name,
					Values: vals,
					Docs:   docs,
				})
			}
		default:
			return nil, fmt.Errorf("%s: unsupported declaration %s", filename, gen.Tok)
		}
	}

	for _, e := range doc.Enums {
		if len(e.Variants) == 0 {
			return nil, fmt.Errorf("%s: enum %s has no variants", filename, e.Name)
		}
		if e.Fallback != "" {
			found := false
			for _, v := range e.Variants {
				if v.Suffix == e.Fallback {
					found = true
					break
				}
			}
			if !found {
				return nil, fmt.Errorf("%s: enum %s fallback %q does not match any variant", filename, e.Name, e.Fallback)
			}
		}
	}
	return doc, nil
}

func commentFor(primary, fallback *ast.CommentGroup) *ast.CommentGroup {
	if primary != nil {
		return primary
	}
	return fallback
}

type directiveSet map[string]string

func (d directiveSet) has(key string) bool {
	_, ok := d[key]
	return ok
}

func (d directiveSet) get(key string) string {
	return d[key]
}

func parseCommentGroup(cg *ast.CommentGroup) ([]string, directiveSet) {
	directives := directiveSet{}
	if cg == nil {
		return nil, directives
	}
	var docs []string
	for _, c := range cg.List {
		line := strings.TrimPrefix(c.Text, "//")
		if rest, ok := strings.CutPrefix(line, "schema:"); ok {
			for _, item := range strings.Split(rest, ",") {
				item = strings.TrimSpace(item)
				if item == "" {
					continue
				}
				if k, v, hasEq := strings.Cut(item, "="); hasEq {
					directives[strings.TrimSpace(k)] = strings.TrimSpace(v)
				} else {
					directives[item] = ""
				}
			}
			continue
		}
		docs = append(docs, strings.TrimPrefix(line, " "))
	}
	return docs, directives
}

func parseTypeExpr(expr ast.Expr) (TypeRef, error) {
	var tr TypeRef
	if star, ok := expr.(*ast.StarExpr); ok {
		tr.Pointer = true
		expr = star.X
	}
	if arr, ok := expr.(*ast.ArrayType); ok {
		if arr.Len == nil {
			tr.Slice = true
		} else {
			lit, ok := arr.Len.(*ast.BasicLit)
			if !ok || lit.Kind != token.INT {
				return tr, fmt.Errorf("array length must be an integer literal")
			}
			n, err := strconv.Atoi(lit.Value)
			if err != nil || n <= 0 {
				return tr, fmt.Errorf("invalid array length %q", lit.Value)
			}
			tr.ArrayLen = n
		}
		expr = arr.Elt
	}
	ident, ok := expr.(*ast.Ident)
	if !ok {
		return tr, fmt.Errorf("unsupported type expression %T", expr)
	}
	tr.Elem = ident.Name
	return tr, nil
}

// EmitGo generates a gofmt-formatted Go file for pkgName.
func EmitGo(doc *Document, pkgName, srcRel string) ([]byte, error) {
	var b strings.Builder
	fmt.Fprintf(&b, "// Code generated by ./xtask schema from %s; DO NOT EDIT.\n\n", srcRel)
	fmt.Fprintf(&b, "package %s\n\n", pkgName)

	if len(doc.Consts) > 0 {
		b.WriteString("const (\n")
		for _, c := range doc.Consts {
			writeGoDocs(&b, "\t", c.Docs)
			if c.GoType != "" {
				fmt.Fprintf(&b, "\t%s %s = %s\n", c.Name, c.GoType, c.Value)
			} else {
				fmt.Fprintf(&b, "\t%s = %s\n", c.Name, c.Value)
			}
		}
		b.WriteString(")\n\n")
	}

	for _, sl := range doc.StringLists {
		writeGoDocs(&b, "", sl.Docs)
		fmt.Fprintf(&b, "var %s = [%d]string{\n", sl.Name, len(sl.Values))
		for _, v := range sl.Values {
			fmt.Fprintf(&b, "\t%q,\n", v)
		}
		b.WriteString("}\n\n")
	}

	for _, ref := range doc.Order {
		switch ref.Kind {
		case DeclEnum:
			emitGoEnum(&b, doc.Enums[ref.Index])
		case DeclStruct:
			emitGoStruct(&b, doc.Structs[ref.Index])
		}
	}

	formatted, err := format.Source([]byte(b.String()))
	if err != nil {
		return nil, fmt.Errorf("gofmt generated Go: %w\n%s", err, b.String())
	}
	return formatted, nil
}

func emitGoEnum(b *strings.Builder, e EnumDef) {
	writeGoDocs(b, "", e.Docs)
	if e.IntEnum {
		fmt.Fprintf(b, "type %s int\n\n", e.Name)
		b.WriteString("const (\n")
		for i, v := range e.Variants {
			writeGoDocs(b, "\t", v.Docs)
			if i == 0 {
				fmt.Fprintf(b, "\t%s %s = iota\n", v.GoName, e.Name)
			} else {
				fmt.Fprintf(b, "\t%s\n", v.GoName)
			}
		}
		b.WriteString(")\n\n")
		namesVar := lowerFirst(e.Name) + "Names"
		fmt.Fprintf(b, "var %s = []string{", namesVar)
		for i, v := range e.Variants {
			if i > 0 {
				b.WriteString(", ")
			}
			fmt.Fprintf(b, "%q", v.WireName)
		}
		b.WriteString("}\n\n")
		fmt.Fprintf(b, "func (s %s) String() string {\n", e.Name)
		fmt.Fprintf(b, "\tif s < 0 || int(s) >= len(%s) {\n", namesVar)
		fmt.Fprintf(b, "\t\treturn %q\n", e.Name+"(?)")
		b.WriteString("\t}\n")
		fmt.Fprintf(b, "\treturn %s[s]\n}\n\n", namesVar)
		fmt.Fprintf(b, "func (s %s) MarshalText() ([]byte, error) {\n", e.Name)
		fmt.Fprintf(b, "\tif s < 0 || int(s) >= len(%s) {\n", namesVar)
		fmt.Fprintf(b, "\t\treturn nil, CoreError(%q)\n", "unknown "+e.Name)
		b.WriteString("\t}\n")
		fmt.Fprintf(b, "\treturn []byte(%s[s]), nil\n}\n\n", namesVar)
		fmt.Fprintf(b, "func (s *%s) UnmarshalText(text []byte) error {\n", e.Name)
		b.WriteString("\tvar name string\n")
		fmt.Fprintf(b, "\tif err := enumUnmarshal(&name, text, %s); err != nil {\n", namesVar)
		b.WriteString("\t\treturn err\n\t}\n")
		fmt.Fprintf(b, "\tfor i, n := range %s {\n", namesVar)
		fmt.Fprintf(b, "\t\tif n == name {\n\t\t\t*s = %s(i)\n\t\t}\n\t}\n", e.Name)
		b.WriteString("\treturn nil\n}\n\n")
		return
	}

	fmt.Fprintf(b, "type %s string\n\n", e.Name)
	b.WriteString("const (\n")
	for _, v := range e.Variants {
		writeGoDocs(b, "\t", v.Docs)
		fmt.Fprintf(b, "\t%s %s = %q\n", v.GoName, e.Name, v.WireName)
	}
	b.WriteString(")\n\n")

	listExpr := ""
	if e.All {
		allName := e.Name + "All"
		fmt.Fprintf(b, "var %s = [%d]%s{\n", allName, len(e.Variants), e.Name)
		for _, v := range e.Variants {
			fmt.Fprintf(b, "\t%s,\n", v.GoName)
		}
		b.WriteString("}\n\n")
		listExpr = allName + "[:]"
	} else {
		varName := lowerFirst(e.Name) + "Values"
		fmt.Fprintf(b, "var %s = []%s{\n", varName, e.Name)
		for _, v := range e.Variants {
			fmt.Fprintf(b, "\t%s,\n", v.GoName)
		}
		b.WriteString("}\n\n")
		listExpr = varName
	}

	fmt.Fprintf(b, "func (v *%s) UnmarshalText(text []byte) error {\n", e.Name)
	if e.Fallback != "" {
		fmt.Fprintf(b, "\tif err := enumUnmarshal(v, text, %s); err != nil {\n", listExpr)
		fmt.Fprintf(b, "\t\t*v = %s%s\n", e.Name, e.Fallback)
		b.WriteString("\t}\n\treturn nil\n}\n\n")
	} else {
		fmt.Fprintf(b, "\treturn enumUnmarshal(v, text, %s)\n}\n\n", listExpr)
	}
}

func emitGoStruct(b *strings.Builder, s StructDef) {
	writeGoDocs(b, "", s.Docs)
	fmt.Fprintf(b, "type %s struct {\n", s.Name)
	for _, f := range s.Fields {
		writeGoDocs(b, "\t", f.Docs)
		tag := f.JSONName
		if f.OmitEmpty {
			tag += ",omitempty"
		}
		if f.Default {
			tag += ",default"
		}
		if f.NonNull {
			tag += ",nonnull"
		}
		fmt.Fprintf(b, "\t%s %s `json:%q`\n", f.Name, f.Type.goString(), tag)
	}
	b.WriteString("}\n\n")
	if s.AllowUnknown {
		fmt.Fprintf(b, "func (%s) AllowUnknownJSONFields() {}\n\n", s.Name)
	}
	if s.EmptyCheck {
		fmt.Fprintf(b, "func (v %s) IsEmpty() bool {\n\treturn ", s.Name)
		for i, f := range s.Fields {
			if i > 0 {
				b.WriteString(" && ")
			}
			fmt.Fprintf(b, "v.%s == nil", f.Name)
		}
		b.WriteString("\n}\n\n")
	}
}

func (t TypeRef) goString() string {
	var b strings.Builder
	if t.Pointer {
		b.WriteByte('*')
	}
	if t.Slice {
		b.WriteString("[]")
	} else if t.ArrayLen > 0 {
		fmt.Fprintf(&b, "[%d]", t.ArrayLen)
	}
	b.WriteString(t.Elem)
	return b.String()
}

func writeGoDocs(b *strings.Builder, indent string, docs []string) {
	for _, line := range docs {
		if line == "" {
			fmt.Fprintf(b, "%s//\n", indent)
		} else {
			fmt.Fprintf(b, "%s// %s\n", indent, line)
		}
	}
}

// EmitRust generates a rustfmt-clean Rust source file.
func EmitRust(doc *Document, srcRel string) ([]byte, error) {
	structByName := map[string]StructDef{}
	for _, s := range doc.Structs {
		structByName[s.Name] = s
	}
	floatCache := map[string]bool{}
	var hasFloat func(name string) bool
	hasFloat = func(name string) bool {
		if v, ok := floatCache[name]; ok {
			return v
		}
		if name == "float32" || name == "float64" {
			return true
		}
		s, ok := structByName[name]
		if !ok {
			return false
		}
		// Break potential cycles conservatively.
		floatCache[name] = false
		for _, f := range s.Fields {
			if hasFloat(f.Type.Elem) {
				floatCache[name] = true
				return true
			}
		}
		return false
	}

	needsNonNull := false
	needsIsZeroUsize := false
	for _, s := range doc.Structs {
		for _, f := range s.Fields {
			if f.NonNull {
				needsNonNull = true
			}
			if f.OmitEmpty && !f.Type.Pointer && !f.Type.Slice && f.Type.Elem == "uint" {
				needsIsZeroUsize = true
			}
		}
	}

	var b strings.Builder
	fmt.Fprintf(&b, "// Code generated by ./xtask schema from %s; DO NOT EDIT.\n\n", srcRel)
	if len(doc.Enums) > 0 || len(doc.Structs) > 0 {
		b.WriteString("use serde::{Deserialize, Serialize};\n\n")
	}

	for _, c := range doc.Consts {
		writeRustDocs(&b, "", c.Docs)
		rustType := rustConstType(c)
		fmt.Fprintf(&b, "pub const %s: %s = %s;\n", toScreamingSnake(c.Name), rustType, c.Value)
	}
	if len(doc.Consts) > 0 {
		b.WriteByte('\n')
	}

	for _, sl := range doc.StringLists {
		writeRustDocs(&b, "", sl.Docs)
		fmt.Fprintf(&b, "pub const %s: [&str; %d] = [\n", toScreamingSnake(sl.Name), len(sl.Values))
		for _, v := range sl.Values {
			fmt.Fprintf(&b, "    %q,\n", v)
		}
		b.WriteString("];\n\n")
	}

	for _, ref := range doc.Order {
		switch ref.Kind {
		case DeclEnum:
			emitRustEnum(&b, doc.Enums[ref.Index])
		case DeclStruct:
			s := doc.Structs[ref.Index]
			emitRustStruct(&b, s, !hasFloat(s.Name), structByName)
		}
	}

	if needsNonNull {
		b.WriteString("fn deserialize_nonnull<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(\n")
		b.WriteString("    deserializer: D,\n")
		b.WriteString(") -> std::result::Result<Option<T>, D::Error> {\n")
		b.WriteString("    T::deserialize(deserializer).map(Some)\n")
		b.WriteString("}\n\n")
	}
	if needsIsZeroUsize {
		b.WriteString("fn is_zero_usize(n: &usize) -> bool {\n")
		b.WriteString("    *n == 0\n")
		b.WriteString("}\n")
	}

	return []byte(strings.TrimRight(b.String(), "\n") + "\n"), nil
}

func rustConstType(c ConstDef) string {
	if c.GoType != "" {
		return goScalarToRust(c.GoType)
	}
	switch c.Kind {
	case token.INT:
		return "usize"
	case token.FLOAT:
		return "f64"
	case token.STRING:
		return "&str"
	default:
		return "usize"
	}
}

func emitRustEnum(b *strings.Builder, e EnumDef) {
	rustName := toRustTypeName(e.Name)
	writeRustDocs(b, "", e.Docs)
	derives := []string{"Clone", "Copy", "Debug"}
	if e.IntEnum {
		derives = append(derives, "Default")
	}
	derives = append(derives, "PartialEq", "Eq", "Hash", "Serialize", "Deserialize")
	fmt.Fprintf(b, "#[derive(%s)]\n", strings.Join(derives, ", "))
	fmt.Fprintf(b, "pub enum %s {\n", rustName)
	for i, v := range e.Variants {
		writeRustDocs(b, "    ", v.Docs)
		if e.IntEnum && i == 0 {
			b.WriteString("    #[default]\n")
		}
		if e.Fallback != "" && v.Suffix == e.Fallback {
			fmt.Fprintf(b, "    #[serde(rename = %q, other)]\n", v.WireName)
		} else {
			fmt.Fprintf(b, "    #[serde(rename = %q)]\n", v.WireName)
		}
		fmt.Fprintf(b, "    %s,\n", toRustTypeName(v.Suffix))
	}
	b.WriteString("}\n\n")
	if e.All {
		fmt.Fprintf(b, "impl %s {\n", rustName)
		fmt.Fprintf(b, "    pub const ALL: [Self; %d] = [\n", len(e.Variants))
		for _, v := range e.Variants {
			fmt.Fprintf(b, "        Self::%s,\n", toRustTypeName(v.Suffix))
		}
		b.WriteString("    ];\n}\n\n")
	}
}

func emitRustStruct(b *strings.Builder, s StructDef, isEq bool, structs map[string]StructDef) {
	rustName := toRustTypeName(s.Name)
	writeRustDocs(b, "", s.Docs)
	derives := []string{"Clone"}
	if s.Copy {
		derives = append(derives, "Copy")
	}
	derives = append(derives, "Debug")
	if s.Default {
		derives = append(derives, "Default")
	}
	derives = append(derives, "PartialEq")
	if isEq {
		derives = append(derives, "Eq")
	}
	derives = append(derives, "Serialize", "Deserialize")
	fmt.Fprintf(b, "#[derive(%s)]\n", strings.Join(derives, ", "))
	if !s.AllowUnknown {
		b.WriteString("#[serde(deny_unknown_fields)]\n")
	}
	fmt.Fprintf(b, "pub struct %s {\n", rustName)
	for _, f := range s.Fields {
		writeRustDocs(b, "    ", f.Docs)
		rustField := toSnakeCase(f.Name)
		var attrs []string
		if f.JSONName != rustField {
			attrs = append(attrs, fmt.Sprintf("rename = %q", f.JSONName))
		}
		if f.Default || (f.Type.Pointer && f.OmitEmpty) {
			attrs = append(attrs, "default")
		}
		if f.OmitEmpty {
			switch {
			case f.Type.Pointer:
				attrs = append(attrs, `skip_serializing_if = "Option::is_none"`)
			case f.Type.Slice:
				attrs = append(attrs, `skip_serializing_if = "Vec::is_empty"`)
			case f.Type.Elem == "uint":
				attrs = append(attrs, `skip_serializing_if = "is_zero_usize"`)
			default:
				if nested, ok := structs[f.Type.Elem]; ok && nested.EmptyCheck {
					attrs = append(attrs, fmt.Sprintf(`skip_serializing_if = "%s::is_empty"`, toRustTypeName(nested.Name)))
				}
			}
		}
		if f.NonNull {
			attrs = append(attrs, `deserialize_with = "deserialize_nonnull"`)
		}
		if len(attrs) > 0 {
			joined := strings.Join(attrs, ", ")
			if len(joined) > 70 {
				b.WriteString("    #[serde(\n")
				for i, a := range attrs {
					if i < len(attrs)-1 {
						fmt.Fprintf(b, "        %s,\n", a)
					} else {
						fmt.Fprintf(b, "        %s\n", a)
					}
				}
				b.WriteString("    )]\n")
			} else {
				fmt.Fprintf(b, "    #[serde(%s)]\n", joined)
			}
		}
		fmt.Fprintf(b, "    pub %s: %s,\n", rustField, f.Type.rustString())
	}
	b.WriteString("}\n\n")
	if s.EmptyCheck {
		fmt.Fprintf(b, "impl %s {\n", rustName)
		b.WriteString("    pub fn is_empty(&self) -> bool {\n")
		for i, f := range s.Fields {
			if i == 0 {
				fmt.Fprintf(b, "        self.%s.is_none()\n", toSnakeCase(f.Name))
			} else {
				fmt.Fprintf(b, "            && self.%s.is_none()\n", toSnakeCase(f.Name))
			}
		}
		b.WriteString("    }\n}\n\n")
	}
}

func (t TypeRef) rustString() string {
	elem := goScalarToRust(t.Elem)
	if elem == "" {
		elem = toRustTypeName(t.Elem)
	}
	inner := elem
	if t.Slice {
		inner = "Vec<" + elem + ">"
	} else if t.ArrayLen > 0 {
		inner = fmt.Sprintf("[%s; %d]", elem, t.ArrayLen)
	}
	if t.Pointer {
		return "Option<" + inner + ">"
	}
	return inner
}

func goScalarToRust(name string) string {
	switch name {
	case "bool":
		return "bool"
	case "string":
		return "String"
	case "uint8":
		return "u8"
	case "uint16":
		return "u16"
	case "uint32":
		return "u32"
	case "uint64":
		return "u64"
	case "uint":
		return "usize"
	case "int32":
		return "i32"
	case "int64":
		return "i64"
	case "float32":
		return "f32"
	case "float64":
		return "f64"
	default:
		return ""
	}
}

func writeRustDocs(b *strings.Builder, indent string, docs []string) {
	for _, line := range docs {
		if line == "" {
			fmt.Fprintf(b, "%s///\n", indent)
		} else {
			fmt.Fprintf(b, "%s/// %s\n", indent, line)
		}
	}
}

// splitWords splits a Go CamelCase identifier (including initialisms like
// EQSettings, USBStatus, TargetIDs, WorkerCPU, InputGainDB, USB1) into words.
func splitWords(name string) []string {
	runes := []rune(name)
	var words []string
	start := 0
	for i := 1; i < len(runes); i++ {
		prev, cur := runes[i-1], runes[i]
		nextIsLower := i+1 < len(runes) && unicode.IsLower(runes[i+1])
		// Keep pluralized initialism "IDs" together as one word ("IDs" -> "ids").
		if i+1 == len(runes)-1 && cur == 's' && unicode.IsUpper(prev) {
			continue
		}
		if unicode.IsUpper(cur) {
			if unicode.IsLower(prev) || unicode.IsDigit(prev) || (unicode.IsUpper(prev) && nextIsLower && !(cur == 'I' && i+2 == len(runes) && runes[i+1] == 's')) {
				// Avoid splitting "IDs" between I and D before lowercase 's'.
				if prev == 'I' && cur == 'D' && i+1 < len(runes) && runes[i+1] == 's' && (i+2 == len(runes) || !unicode.IsLower(runes[i+2])) {
					continue
				}
				words = append(words, string(runes[start:i]))
				start = i
			}
		}
	}
	words = append(words, string(runes[start:]))
	return words
}

// toRustTypeName converts Go CamelCase with uppercase acronyms into Rust
// UpperCamelCase (EQSettings -> EqSettings, USBGadget -> UsbGadget).
func toRustTypeName(name string) string {
	var b strings.Builder
	for _, w := range splitWords(name) {
		lower := strings.ToLower(w)
		r := []rune(lower)
		if len(r) > 0 {
			r[0] = unicode.ToUpper(r[0])
		}
		b.WriteString(string(r))
	}
	return b.String()
}

func toSnakeCase(name string) string {
	words := splitWords(name)
	for i, w := range words {
		words[i] = strings.ToLower(w)
	}
	return strings.Join(words, "_")
}

func toScreamingSnake(name string) string {
	return strings.ToUpper(toSnakeCase(name))
}

func lowerFirst(s string) string {
	if s == "" {
		return ""
	}
	// Lowercase leading acronym cleanly: USBLinkState -> usbLinkState, ImageStatus -> imageStatus.
	words := splitWords(s)
	words[0] = strings.ToLower(words[0])
	return strings.Join(words, "")
}
