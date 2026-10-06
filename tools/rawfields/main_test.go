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
		{Definition: "dup.T", Property: "r", Kind: "json"},
		{Definition: "sample.Part", Property: "Untagged", Kind: "json"},
		{Definition: "sample.Part", Property: "args", Kind: "json"},
		{Definition: "sample.Part", Property: "data", Kind: "bytes"},
		{Definition: "sample.Part", Property: "result", Kind: "json"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("scan() = %#v, want %#v", got, want)
	}
}

func TestSkipsDotDirectories(t *testing.T) {
	t.Parallel()
	got, err := scan("testdata")
	if err != nil {
		t.Fatal(err)
	}
	// Verify sample.Part still found exactly as before (dotted .worktrees dir skipped)
	sampleParts := []field{}
	for _, f := range got {
		if f.Definition == "sample.Part" {
			sampleParts = append(sampleParts, f)
		}
	}
	want := []field{
		{Definition: "sample.Part", Property: "Untagged", Kind: "json"},
		{Definition: "sample.Part", Property: "args", Kind: "json"},
		{Definition: "sample.Part", Property: "data", Kind: "bytes"},
		{Definition: "sample.Part", Property: "result", Kind: "json"},
	}
	if !reflect.DeepEqual(sampleParts, want) {
		t.Fatalf("sample.Part fields = %#v, want %#v", sampleParts, want)
	}
}

func TestDeduplicatesIdenticalEntries(t *testing.T) {
	t.Parallel()
	got, err := scan("testdata/dup")
	if err != nil {
		t.Fatal(err)
	}
	want := []field{
		{Definition: "dup.T", Property: "r", Kind: "json"},
	}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("scan() = %#v, want %#v", got, want)
	}
}
