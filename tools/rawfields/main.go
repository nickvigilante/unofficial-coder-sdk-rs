// Command rawfields lists struct fields typed json.RawMessage or []byte in a
// Go source tree, keyed the way swaggo names Swagger definitions.
package main

import (
	"encoding/json"
	"fmt"
	"go/ast"
	"go/parser"
	"go/token"
	"io/fs"
	"os"
	"path/filepath"
	"reflect"
	"sort"
	"strings"
)

type field struct {
	Definition string `json:"definition"`
	Property   string `json:"property"`
	Kind       string `json:"kind"`
}

var skipDirs = map[string]bool{"node_modules": true, "vendor": true, "site": true}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: rawfields <source-dir>")
		os.Exit(2)
	}
	fields, err := scan(os.Args[1])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	enc := json.NewEncoder(os.Stdout)
	enc.SetIndent("", "  ")
	if err := enc.Encode(fields); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func scan(root string) ([]field, error) {
	fields := []field{}
	fset := token.NewFileSet()
	err := filepath.WalkDir(root, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if d.IsDir() {
			// Skip explicit dirs and any dot-prefixed dir except at root
			if skipDirs[d.Name()] {
				return filepath.SkipDir
			}
			if strings.HasPrefix(d.Name(), ".") && path != root {
				return filepath.SkipDir
			}
			return nil
		}
		if !strings.HasSuffix(path, ".go") || strings.HasSuffix(path, "_test.go") {
			return nil
		}
		file, err := parser.ParseFile(fset, path, nil, parser.SkipObjectResolution)
		if err != nil {
			return fmt.Errorf("parse %s: %w", path, err)
		}
		fields = append(fields, fileFields(file)...)
		return nil
	})
	if err != nil {
		return nil, err
	}
	sort.Slice(fields, func(i, j int) bool {
		if fields[i].Definition != fields[j].Definition {
			return fields[i].Definition < fields[j].Definition
		}
		return fields[i].Property < fields[j].Property
	})
	// Deduplicate identical entries
	seen := make(map[[3]string]bool)
	unique := []field{}
	for _, f := range fields {
		key := [3]string{f.Definition, f.Property, f.Kind}
		if !seen[key] {
			seen[key] = true
			unique = append(unique, f)
		}
	}
	return unique, nil
}

func fileFields(file *ast.File) []field {
	var out []field
	pkg := file.Name.Name
	for _, decl := range file.Decls {
		gen, ok := decl.(*ast.GenDecl)
		if !ok || gen.Tok != token.TYPE {
			continue
		}
		for _, spec := range gen.Specs {
			ts := spec.(*ast.TypeSpec)
			st, ok := ts.Type.(*ast.StructType)
			if !ok {
				continue
			}
			for _, f := range st.Fields.List {
				if len(f.Names) == 0 {
					continue
				}
				kind := rawKind(f.Type)
				if kind == "" {
					continue
				}
				tag := reflect.StructTag("")
				if f.Tag != nil {
					tag = reflect.StructTag(strings.Trim(f.Tag.Value, "`"))
				}
				if tag.Get("swaggertype") != "" {
					continue
				}
				name := strings.Split(tag.Get("json"), ",")[0]
				if name == "-" {
					continue
				}
				if name == "" {
					name = f.Names[0].Name
				}
				out = append(out, field{Definition: pkg + "." + ts.Name.Name, Property: name, Kind: kind})
			}
		}
	}
	return out
}

func rawKind(expr ast.Expr) string {
	if star, ok := expr.(*ast.StarExpr); ok {
		expr = star.X
	}
	switch t := expr.(type) {
	case *ast.SelectorExpr:
		if id, ok := t.X.(*ast.Ident); ok && id.Name == "json" && t.Sel.Name == "RawMessage" {
			return "json"
		}
	case *ast.ArrayType:
		if id, ok := t.Elt.(*ast.Ident); ok && t.Len == nil && id.Name == "byte" {
			return "bytes"
		}
	}
	return ""
}
