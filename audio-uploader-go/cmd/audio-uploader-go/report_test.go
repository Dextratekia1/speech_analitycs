package main

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"
)

// TestReasonToErrorCode_KnownMappings verifies that every known reason string
// maps to the expected structured error-code constant, and that unknown strings
// map to errorCodeUnknown.
func TestReasonToErrorCode_KnownMappings(t *testing.T) {
	tests := []struct {
		name   string
		reason string
		want   string
	}{
		{"empty", "", errorCodeNone},
		{"read json prefix", "read json: permission denied", errorCodeJSONRead},
		{"parse json prefix", "parse json: invalid character", errorCodeJSONParse},
		{"lookup_ok=false exact", "lookup_ok=false", errorCodeLookupNotOK},
		{"descrip_rpta vacío exact", "descrip_rpta vacío", errorCodeDescripRptaEmpty},
		{"descrip_rpta=OTRO exact", "descrip_rpta=OTRO", errorCodeDescripRptaOTRO},
		{"campo requerido prefix", "campo requerido faltante/vacío: documento", errorCodeMissingRequiredField},
		{"campo opcional prefix", "campo opcional vacío: observacion", errorCodeEmptyOptionalField},
		{"ffprobe_ok=false exact", "ffprobe_ok=false", errorCodeFFProbeNotOK},
		{"marshal out prefix", "marshal out: unsupported value", errorCodeJSONPrepare},
		{"write prepared prefix", "write prepared: permission denied", errorCodeJSONPrepare},
		{"upload json prefix", "upload json: connection reset", errorCodeSFTPJSON},
		{"upload wav prefix", "upload wav: connection reset", errorCodeSFTPWAV},
		{"unknown", "unexpected future reason", errorCodeUnknown},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got := reasonToErrorCode(tt.reason)
			if got != tt.want {
				t.Errorf("reasonToErrorCode(%q) = %q, want %q", tt.reason, got, tt.want)
			}
		})
	}
}

// TestUploadReport_JSONIncludesV2Fields verifies that UploadReport marshals to
// JSON with all new v2 fields present alongside existing backward-compatible fields.
func TestUploadReport_JSONIncludesV2Fields(t *testing.T) {
	report := UploadReport{
		SchemaVersion: 2,
		Client:        "natura",
		Date:          "2026-01-08",
		RunID:         "test-run",
		DryRun:        true,
		StartedAt:     "2026-01-08T10:00:00Z",
		FinishedAt:    "2026-01-08T10:00:01Z",
		DurationMs:    1000,
		Total:         2,
		Valid:          1,
		Skipped:       1,
		Counts: UploadCounts{
			Total:             2,
			SkippedParse:      0,
			SkippedValidation: 1,
			SkippedPrepare:    0,
			SentOK:            0,
			SendError:         0,
		},
		Items: []UploadItem{
			{
				RecordID:  "synthetic-001",
				JsonIn:    "matched/synthetic-001.json",
				JsonOut:   "prepared/json/synthetic-001.json",
				WavPath:   "wav/synthetic-001.wav",
				SendOK:    true,
				Status:    statusPrepared,
				ErrorCode: errorCodeNone,
			},
			{
				RecordID:  "synthetic-002",
				JsonIn:    "matched/synthetic-002.json",
				WavPath:   "wav/synthetic-002.wav",
				SendOK:    false,
				Reason:    "lookup_ok=false",
				Status:    statusSkippedValidation,
				ErrorCode: errorCodeLookupNotOK,
			},
		},
	}

	b, err := json.Marshal(report)
	if err != nil {
		t.Fatalf("json.Marshal failed: %v", err)
	}

	var m map[string]interface{}
	if err := json.Unmarshal(b, &m); err != nil {
		t.Fatalf("json.Unmarshal failed: %v", err)
	}

	// New v2 fields.
	if v, ok := m["schema_version"]; !ok {
		t.Error("schema_version missing from JSON")
	} else if int(v.(float64)) != 2 {
		t.Errorf("schema_version = %v, want 2", v)
	}
	if _, ok := m["started_at"]; !ok {
		t.Error("started_at missing from JSON")
	}
	if _, ok := m["finished_at"]; !ok {
		t.Error("finished_at missing from JSON")
	}
	if v, ok := m["duration_ms"]; !ok {
		t.Error("duration_ms missing from JSON")
	} else if int64(v.(float64)) != 1000 {
		t.Errorf("duration_ms = %v, want 1000", v)
	}
	if _, ok := m["counts"]; !ok {
		t.Error("counts missing from JSON")
	}
	if _, ok := m["items"]; !ok {
		t.Error("items missing from JSON")
	}

	// Existing backward-compatible fields must still be present.
	for _, field := range []string{"client", "date", "run_id", "dry_run", "total", "valid", "skipped"} {
		if _, ok := m[field]; !ok {
			t.Errorf("existing backward-compatible field %q missing from JSON", field)
		}
	}
}

// TestUploadCounts_JSONFieldNames verifies that UploadCounts serializes with
// correct snake_case field names and no CamelCase names leak through.
func TestUploadCounts_JSONFieldNames(t *testing.T) {
	counts := UploadCounts{
		Total:             5,
		SkippedParse:      1,
		SkippedValidation: 2,
		SkippedPrepare:    0,
		SentOK:            1,
		SendError:         1,
	}

	b, err := json.Marshal(counts)
	if err != nil {
		t.Fatalf("json.Marshal failed: %v", err)
	}

	var m map[string]interface{}
	if err := json.Unmarshal(b, &m); err != nil {
		t.Fatalf("json.Unmarshal failed: %v", err)
	}

	// Required snake_case field names.
	for _, field := range []string{"total", "skipped_parse", "skipped_validation", "skipped_prepare", "sent_ok", "send_error"} {
		if _, ok := m[field]; !ok {
			t.Errorf("expected JSON field %q not found in UploadCounts output", field)
		}
	}

	// CamelCase Go field names must not appear in JSON output.
	for _, field := range []string{"Total", "SkippedParse", "SkippedValidation", "SkippedPrepare", "SentOK", "SendError"} {
		if _, ok := m[field]; ok {
			t.Errorf("unexpected CamelCase key %q found in UploadCounts JSON output", field)
		}
	}
}

// TestUploadItem_JSONIncludesStatusAndErrorCode verifies that UploadItem
// serializes status and error_code alongside all existing fields.
func TestUploadItem_JSONIncludesStatusAndErrorCode(t *testing.T) {
	item := UploadItem{
		RecordID:  "synthetic-record",
		JsonIn:    "matched/synthetic-record.json",
		JsonOut:   "prepared/json/synthetic-record.json",
		WavPath:   "wav/synthetic-record.wav",
		SendOK:    false,
		Reason:    "lookup_ok=false",
		Status:    statusSkippedValidation,
		ErrorCode: errorCodeLookupNotOK,
	}

	b, err := json.Marshal(item)
	if err != nil {
		t.Fatalf("json.Marshal failed: %v", err)
	}

	var m map[string]interface{}
	if err := json.Unmarshal(b, &m); err != nil {
		t.Fatalf("json.Unmarshal failed: %v", err)
	}

	if v, ok := m["status"]; !ok {
		t.Error("status missing from UploadItem JSON")
	} else if v != "skipped_validation" {
		t.Errorf("status = %q, want %q", v, "skipped_validation")
	}
	if v, ok := m["error_code"]; !ok {
		t.Error("error_code missing from UploadItem JSON")
	} else if v != "lookup_not_ok" {
		t.Errorf("error_code = %q, want %q", v, "lookup_not_ok")
	}

	// Existing fields must still be present.
	for _, field := range []string{"record_id", "json_in", "json_out", "wav_path", "send_ok", "reason"} {
		if _, ok := m[field]; !ok {
			t.Errorf("existing UploadItem field %q missing from JSON", field)
		}
	}
}

// TestUploadItem_StatusAndErrorCodeAlwaysEmitted verifies that status and
// error_code appear in JSON even when both are empty strings, confirming
// that neither field has omitempty.
func TestUploadItem_StatusAndErrorCodeAlwaysEmitted(t *testing.T) {
	item := UploadItem{
		Status:    "",
		ErrorCode: "",
	}

	b, err := json.Marshal(item)
	if err != nil {
		t.Fatalf("json.Marshal failed: %v", err)
	}

	var m map[string]interface{}
	if err := json.Unmarshal(b, &m); err != nil {
		t.Fatalf("json.Unmarshal failed: %v", err)
	}

	if _, ok := m["status"]; !ok {
		t.Error("status must always be emitted (no omitempty) but is absent when empty")
	}
	if _, ok := m["error_code"]; !ok {
		t.Error("error_code must always be emitted (no omitempty) but is absent when empty")
	}
}

// TestUploadReport_NoNewPIIFieldNames guards against PII field names being
// added to UploadReport, UploadItem, or UploadCounts by inspecting json struct tags.
func TestUploadReport_NoNewPIIFieldNames(t *testing.T) {
	forbidden := []string{
		"telefono", "phone",
		"nombre_deudor", "deudor", "deuda", "monto_deuda", "saldo",
		"agent_name", "nombre_agente",
		"password", "sftp_password",
		"host_key", "sftp_host_key",
	}

	// Existing path/id fields inherited from upstream filename conventions.
	allowed := map[string]bool{
		"record_id": true,
		"json_in":   true,
		"json_out":  true,
		"wav_path":  true,
	}

	checkType := func(typ reflect.Type, typeName string) {
		for i := 0; i < typ.NumField(); i++ {
			field := typ.Field(i)
			tag := field.Tag.Get("json")
			if tag == "" {
				continue
			}
			jsonName := strings.Split(tag, ",")[0]
			if jsonName == "" || jsonName == "-" {
				continue
			}
			if allowed[jsonName] {
				continue
			}
			for _, bad := range forbidden {
				if jsonName == bad {
					t.Errorf("%s.%s has forbidden PII json tag %q", typeName, field.Name, jsonName)
				}
			}
		}
	}

	checkType(reflect.TypeOf(UploadReport{}), "UploadReport")
	checkType(reflect.TypeOf(UploadItem{}), "UploadItem")
	checkType(reflect.TypeOf(UploadCounts{}), "UploadCounts")
}

// TestUploadReport_StatusConstants verifies all status constants equal their
// designed string values, catching any accidental redefinition.
func TestUploadReport_StatusConstants(t *testing.T) {
	cases := []struct {
		name string
		got  string
		want string
	}{
		{"statusSent", statusSent, "sent"},
		{"statusPrepared", statusPrepared, "prepared"},
		{"statusSkippedParse", statusSkippedParse, "skipped_parse"},
		{"statusSkippedValidation", statusSkippedValidation, "skipped_validation"},
		{"statusSkippedPrepare", statusSkippedPrepare, "skipped_prepare"},
		{"statusSendError", statusSendError, "send_error"},
	}
	for _, c := range cases {
		if c.got != c.want {
			t.Errorf("%s = %q, want %q", c.name, c.got, c.want)
		}
	}
}
