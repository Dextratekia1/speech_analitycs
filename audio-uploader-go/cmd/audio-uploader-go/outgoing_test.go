package main

import (
	"sort"
	"strings"
	"testing"
)

// boolPtr and float64Ptr are helpers for pointer fields in Matched.
func boolPtr(b bool) *bool       { return &b }
func float64Ptr(f float64) *float64 { return &f }

// containsStr reports whether slice contains s.
func containsStr(slice []string, s string) bool {
	for _, v := range slice {
		if v == s {
			return true
		}
	}
	return false
}

// makeNaturaMatched returns a Matched with all fields required by buildOutgoing +
// validateOutgoing for the natura client. All values are synthetic.
func makeNaturaMatched() *Matched {
	m := &Matched{}
	m.Client.ID = 52
	m.Client.Code = "natura"
	m.Call.Telefono = "987654321"
	m.Call.IDAgente = 1
	m.Call.FechaGestion = "2026-01-08"
	m.Call.Hora = "14:30:22"
	m.Lookup.OK = true
	m.Rpta.DescripRpta = "GESTIÓN EFECTIVA"
	m.Probe.FfprobeOK = boolPtr(true)
	m.Probe.DurationSec = float64Ptr(45.2)
	agentName := "Agente Prueba"
	m.Agent.NombreAgente = &agentName
	m.Data = map[string]any{
		"ciclo":         float64(3),
		"codigo_deudor": "CD-00001",
		"deuda_total":   float64(1500),
		"dias_atraso":   float64(30),
		"monto_campana": float64(500),
		"nombre_deudor": "Deudor Prueba",
		"nombre_rpta":   "ACEPTA",
		"observacion":   "sin observaciones",
	}
	return m
}

// makeMafMatched returns a Matched with all fields required by buildOutgoing +
// validateOutgoing for the maf client (moneda=1 by default). All values are synthetic.
func makeMafMatched() *Matched {
	m := &Matched{}
	m.Client.ID = 59
	m.Client.Code = "maf"
	m.Call.Telefono = "987654321"
	m.Call.IDAgente = 7
	m.Call.FechaGestion = "2026-01-08"
	m.Call.Hora = "14:30:22"
	m.Lookup.OK = true
	m.Rpta.DescripRpta = "PAGO REALIZADO"
	m.Probe.FfprobeOK = boolPtr(true)
	m.Probe.DurationSec = float64Ptr(62.5)
	agentName := "Agente Maf Prueba"
	m.Agent.NombreAgente = &agentName
	m.Data = map[string]any{
		"n_cuota":            float64(5),
		"concatenado":        "MAF-20260108",
		"fec_vencimiento":    "2026-01-15",
		"moneda":             float64(1),
		"monto_cuota":        float64(250),
		"ultimo_tramo":       "SI",
		"categoria":          "A",
		"placa":              "TESTPL",
		"documento":          "DOC-00001",
		"deudor":             "Deudor MAF Prueba",
		"nid_opecodout":      "NID-001",
		"nombre_rpta":        "ACEPTA",
		"observacion":        "ok",
		"fec_comp":           "2026-02-01",
		"monto_comp_soles":   float64(100),
		"monto_comp_dolares": float64(25),
	}
	return m
}

// --- buildOutgoing tests ---

func TestBuildOutgoing_Natura_ContainsExpectedFields(t *testing.T) {
	m := makeNaturaMatched()
	out, required, _ := buildOutgoing(m)

	if out["cliente"] != "natura" {
		t.Errorf("expected cliente=natura, got %v", out["cliente"])
	}
	if out["telefono"] != "987654321" {
		t.Errorf("expected telefono=987654321, got %v", out["telefono"])
	}
	if out["descrip_rpta"] != "GESTIÓN EFECTIVA" {
		t.Errorf("expected descrip_rpta from Rpta field, got %v", out["descrip_rpta"])
	}

	// Natura-specific required data fields must be in both out and required.
	for _, k := range []string{"ciclo", "codigo_deudor", "deuda_total", "dias_atraso", "monto_campana", "nombre_deudor"} {
		if _, ok := out[k]; !ok {
			t.Errorf("expected natura key %q in out", k)
		}
		if !containsStr(required, k) {
			t.Errorf("expected natura key %q in required", k)
		}
	}
}

func TestBuildOutgoing_Natura_OptionalNotInRequired(t *testing.T) {
	// monto_compromiso and fecha_compromiso are optional; they must not appear in required.
	m := makeNaturaMatched()
	_, required, optional := buildOutgoing(m)
	for _, k := range []string{"monto_compromiso", "fecha_compromiso"} {
		if !containsStr(optional, k) {
			t.Errorf("expected %q in optional", k)
		}
		if containsStr(required, k) {
			t.Errorf("%q must not be in required (it is optional)", k)
		}
	}
}

func TestBuildOutgoing_Maf_Moneda1_Soles(t *testing.T) {
	m := makeMafMatched()
	m.Data["moneda"] = float64(1)
	out, _, _ := buildOutgoing(m)
	if out["moneda"] != "SOLES" {
		t.Errorf("moneda=1 should map to SOLES, got %v", out["moneda"])
	}
}

func TestBuildOutgoing_Maf_Moneda2_Dolares(t *testing.T) {
	m := makeMafMatched()
	m.Data["moneda"] = float64(2)
	out, _, _ := buildOutgoing(m)
	if out["moneda"] != "DOLARES" {
		t.Errorf("moneda=2 should map to DOLARES, got %v", out["moneda"])
	}
}

func TestBuildOutgoing_Maf_MonedaUnknown_Nil(t *testing.T) {
	// Unknown moneda (3) → nil. This is a required field so validation will reject.
	m := makeMafMatched()
	m.Data["moneda"] = float64(3)
	out, _, _ := buildOutgoing(m)
	if out["moneda"] != nil {
		t.Errorf("unknown moneda should map to nil, got %v", out["moneda"])
	}
}

func TestBuildOutgoing_Maf_MontoDolares_DefaultsToZero(t *testing.T) {
	// When monto_comp_dolares is absent, out["monto_compromiso_dolares"] = 0.0.
	m := makeMafMatched()
	delete(m.Data, "monto_comp_dolares")
	out, _, _ := buildOutgoing(m)
	if out["monto_compromiso_dolares"] != 0.0 {
		t.Errorf("missing monto_comp_dolares should default to 0.0, got %v", out["monto_compromiso_dolares"])
	}
}

func TestBuildOutgoing_Maf_FecCompMapsToFechaCompromiso(t *testing.T) {
	// fec_comp in data → fecha_compromiso in out (renaming).
	m := makeMafMatched()
	m.Data["fec_comp"] = "2026-03-01"
	out, _, _ := buildOutgoing(m)
	if out["fecha_compromiso"] != "2026-03-01" {
		t.Errorf("fec_comp should map to fecha_compromiso, got %v", out["fecha_compromiso"])
	}
}

func TestBuildOutgoing_Default_DataKeysInRequired(t *testing.T) {
	// Unknown client: data keys not already in out are added to out and required.
	m := &Matched{}
	m.Client.Code = "unknownclient"
	m.Data = map[string]any{"custom_field": "unique_value"}
	out, required, _ := buildOutgoing(m)

	if out["custom_field"] != "unique_value" {
		t.Errorf("expected custom_field in out, got %v", out["custom_field"])
	}
	if !containsStr(required, "custom_field") {
		t.Errorf("expected custom_field in required for default client path")
	}
}

func TestBuildOutgoing_RequiredSliceSorted(t *testing.T) {
	// Returned required must be sorted (buildOutgoing deduplicates and sorts).
	m := makeNaturaMatched()
	_, required, _ := buildOutgoing(m)
	if !sort.StringsAreSorted(required) {
		t.Errorf("required slice must be sorted; got %v", required)
	}
}

// --- validateOutgoing tests ---

func TestValidateOutgoing_Valid_Natura(t *testing.T) {
	m := makeNaturaMatched()
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if !ok {
		t.Errorf("expected valid natura payload, got rejected: %s", reason)
	}
	if reason != "" {
		t.Errorf("expected empty reason, got %q", reason)
	}
}

func TestValidateOutgoing_Valid_Maf(t *testing.T) {
	m := makeMafMatched()
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if !ok {
		t.Errorf("expected valid maf payload, got rejected: %s", reason)
	}
}

func TestValidateOutgoing_RejectsLookupFalse(t *testing.T) {
	m := makeNaturaMatched()
	m.Lookup.OK = false
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if ok {
		t.Errorf("expected rejection for lookup_ok=false")
	}
	if reason != "lookup_ok=false" {
		t.Errorf("expected reason 'lookup_ok=false', got %q", reason)
	}
}

func TestValidateOutgoing_RejectsDescripRptaEmpty(t *testing.T) {
	m := makeNaturaMatched()
	m.Rpta.DescripRpta = ""
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if ok {
		t.Errorf("expected rejection for empty descrip_rpta")
	}
	if reason != "descrip_rpta vacío" {
		t.Errorf("expected reason 'descrip_rpta vacío', got %q", reason)
	}
}

func TestValidateOutgoing_RejectsDescripRptaWhitespace(t *testing.T) {
	// strings.TrimSpace is applied before the empty check.
	m := makeNaturaMatched()
	m.Rpta.DescripRpta = "   "
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if ok {
		t.Errorf("expected rejection for whitespace-only descrip_rpta")
	}
	if reason != "descrip_rpta vacío" {
		t.Errorf("expected reason 'descrip_rpta vacío', got %q", reason)
	}
}

func TestValidateOutgoing_RejectsDescripRptaOTRO(t *testing.T) {
	m := makeNaturaMatched()
	m.Rpta.DescripRpta = "OTRO"
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if ok {
		t.Errorf("expected rejection for descrip_rpta=OTRO")
	}
	if reason != "descrip_rpta=OTRO" {
		t.Errorf("expected reason 'descrip_rpta=OTRO', got %q", reason)
	}
}

func TestValidateOutgoing_RejectsMissingRequired(t *testing.T) {
	m := makeNaturaMatched()
	out, required, optional := buildOutgoing(m)
	delete(out, "ciclo") // remove a required natura field
	ok, reason := validateOutgoing(m, out, required, optional)
	if ok {
		t.Errorf("expected rejection for missing required field 'ciclo'")
	}
	if !strings.Contains(reason, "ciclo") {
		t.Errorf("expected reason to mention 'ciclo', got %q", reason)
	}
}

func TestValidateOutgoing_RejectsFfprobeNotOk(t *testing.T) {
	m := makeNaturaMatched()
	m.Probe.FfprobeOK = boolPtr(false)
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if ok {
		t.Errorf("expected rejection for ffprobe_ok=false")
	}
	if reason != "ffprobe_ok=false" {
		t.Errorf("expected reason 'ffprobe_ok=false', got %q", reason)
	}
}

func TestValidateOutgoing_NilFfprobeOK_Passes(t *testing.T) {
	// nil ffprobe_ok does not trigger rejection (only false does).
	m := makeNaturaMatched()
	m.Probe.FfprobeOK = nil
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if !ok {
		t.Errorf("expected valid for nil ffprobe_ok, got rejected: %s", reason)
	}
}
