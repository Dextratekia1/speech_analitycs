package main

import (
	"os"
	"path/filepath"
	"testing"
)

// --- resolveWavFromJson tests ---

func TestResolveWavFromJson_AbsolutePath(t *testing.T) {
	dir := t.TempDir()
	f := filepath.Join(dir, "test.json")
	os.WriteFile(f, []byte(`{"audio":{"wav_path":"/abs/path/audio.wav"}}`), 0o644)

	got := resolveWavFromJson(f, "/rundir", "fallback.wav")
	if got != "/abs/path/audio.wav" {
		t.Errorf("expected /abs/path/audio.wav, got %s", got)
	}
}

func TestResolveWavFromJson_RelativePath(t *testing.T) {
	dir := t.TempDir()
	f := filepath.Join(dir, "test.json")
	os.WriteFile(f, []byte(`{"audio":{"wav_path":"subdir/audio.wav"}}`), 0o644)

	got := resolveWavFromJson(f, "/rundir", "fallback.wav")
	want := filepath.Join("/rundir", "subdir", "audio.wav")
	if got != want {
		t.Errorf("expected %s, got %s", want, got)
	}
}

func TestResolveWavFromJson_MissingWavPath(t *testing.T) {
	// JSON has audio object but no wav_path key → fallback.
	dir := t.TempDir()
	f := filepath.Join(dir, "test.json")
	os.WriteFile(f, []byte(`{"audio":{}}`), 0o644)

	got := resolveWavFromJson(f, "/rundir", "fallback.wav")
	want := filepath.Join("/rundir", "wav", "fallback.wav")
	if got != want {
		t.Errorf("expected %s, got %s", want, got)
	}
}

func TestResolveWavFromJson_EmptyWavPath(t *testing.T) {
	// strings.TrimSpace(wp) == "" → treated as absent → fallback.
	dir := t.TempDir()
	f := filepath.Join(dir, "test.json")
	os.WriteFile(f, []byte(`{"audio":{"wav_path":""}}`), 0o644)

	got := resolveWavFromJson(f, "/rundir", "fallback.wav")
	want := filepath.Join("/rundir", "wav", "fallback.wav")
	if got != want {
		t.Errorf("expected %s, got %s", want, got)
	}
}

func TestResolveWavFromJson_FileNotFound(t *testing.T) {
	got := resolveWavFromJson("/nonexistent/path/test.json", "/rundir", "fallback.wav")
	want := filepath.Join("/rundir", "wav", "fallback.wav")
	if got != want {
		t.Errorf("expected %s, got %s", want, got)
	}
}

func TestResolveWavFromJson_InvalidJSON(t *testing.T) {
	dir := t.TempDir()
	f := filepath.Join(dir, "test.json")
	os.WriteFile(f, []byte("not valid json {{{"), 0o644)

	got := resolveWavFromJson(f, "/rundir", "fallback.wav")
	want := filepath.Join("/rundir", "wav", "fallback.wav")
	if got != want {
		t.Errorf("expected %s, got %s", want, got)
	}
}

func TestResolveWavFromJson_NoAudioKey(t *testing.T) {
	// JSON has no "audio" key at all → fallback.
	dir := t.TempDir()
	f := filepath.Join(dir, "test.json")
	os.WriteFile(f, []byte(`{"other_key":"value"}`), 0o644)

	got := resolveWavFromJson(f, "/rundir", "fallback.wav")
	want := filepath.Join("/rundir", "wav", "fallback.wav")
	if got != want {
		t.Errorf("expected %s, got %s", want, got)
	}
}

// --- asInt tests ---

func TestAsInt(t *testing.T) {
	tests := []struct {
		name     string
		input    any
		expected int
	}{
		{"float64 truncates", float64(3.7), 3},
		{"float32 truncates", float32(2.9), 2},
		{"int passthrough", 42, 42},
		{"int32 cast", int32(10), 10},
		{"int64 cast", int64(999), 999},
		{"string numeric", "7", 7},
		{"string invalid returns 0", "notanumber", 0},
		{"nil returns 0", nil, 0},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := asInt(tt.input)
			if got != tt.expected {
				t.Errorf("asInt(%v) = %d, want %d", tt.input, got, tt.expected)
			}
		})
	}
}

// --- isPresent tests ---

func TestIsPresent(t *testing.T) {
	tests := []struct {
		name     string
		input    any
		expected bool
	}{
		{"nil is absent", nil, false},
		{"empty string is absent", "", false},
		{"whitespace-only is absent", "   ", false},
		{"non-empty string is present", "hello", true},
		{"zero int is present", 0, true},       // non-nil non-string → always present
		{"zero float is present", float64(0), true},
		{"bool false is present", false, true}, // non-nil non-string → present
		{"positive int is present", 42, true},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := isPresent(tt.input)
			if got != tt.expected {
				t.Errorf("isPresent(%v) = %v, want %v", tt.input, got, tt.expected)
			}
		})
	}
}

// --- getData tests ---

func TestGetData_KeyExists(t *testing.T) {
	m := map[string]any{"a": float64(1)}
	v, ok := getData(m, "a")
	if !ok {
		t.Errorf("expected ok=true for existing key")
	}
	if v != float64(1) {
		t.Errorf("expected value 1, got %v", v)
	}
}

func TestGetData_KeyMissing(t *testing.T) {
	m := map[string]any{}
	_, ok := getData(m, "missing")
	if ok {
		t.Errorf("expected ok=false for missing key")
	}
}

func TestGetData_NilValueTreatedAsMissing(t *testing.T) {
	// A key with nil value is treated the same as absent.
	m := map[string]any{"b": nil}
	_, ok := getData(m, "b")
	if ok {
		t.Errorf("expected ok=false for nil value (treated as missing)")
	}
}

func TestGetData_StringValue(t *testing.T) {
	m := map[string]any{"key": "hello"}
	v, ok := getData(m, "key")
	if !ok || v != "hello" {
		t.Errorf("expected (hello, true), got (%v, %v)", v, ok)
	}
}

// --- envOr tests ---

func TestEnvOr_Fallback(t *testing.T) {
	// Unset environment variable → default returned.
	result := envOr("TEST_AUDIO_UPLOADER_KEY_NOTSET_12345", "default_val")
	if result != "default_val" {
		t.Errorf("expected default_val, got %s", result)
	}
}

func TestEnvOr_Present(t *testing.T) {
	// t.Setenv sets the variable for this test and restores it after.
	t.Setenv("TEST_AUDIO_UPLOADER_UNIT_XYZ", "test_value_xyz")
	result := envOr("TEST_AUDIO_UPLOADER_UNIT_XYZ", "default")
	if result != "test_value_xyz" {
		t.Errorf("expected test_value_xyz, got %s", result)
	}
}
