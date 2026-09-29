package main

import (
	"reflect"
	"testing"
)

func TestScanFindsRawFields(t *testing.T) {
	t.Parallel()
	got, err := scan("testdata")
	if err != nil {
		t.Fatal(err)
	}
	want := []field{
		{Definition: "sample.Part", Property: "Untagged", Kind: "json"},
		{Definition: "sample.Part", Property: "args", Kind: "json"},
		{Definition: "sample.Part", Property: "data", Kind: "bytes"},
		{Definition: "sample.Part", Property: "result", Kind: "json"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("scan() = %#v, want %#v", got, want)
	}
}
