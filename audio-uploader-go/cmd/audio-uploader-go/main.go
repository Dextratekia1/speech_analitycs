package main

import (
  "encoding/json"
  "flag"
  "fmt"
  "io"
  "log"
  "os"
  "path/filepath"
  "sort"
  "strings"
  "time"
	"strconv"

  "github.com/pkg/sftp"
  "golang.org/x/crypto/ssh"

)

type Args struct {
  Client     string
  Date       string
  SharedRoot string
  RunID      string
  DryRun     bool
}

func envOr(k, def string) string {
  if v := os.Getenv(k); v != "" {
    return v
  }
  return def
}

func readSecretEnv(path string) {
  b, err := os.ReadFile(path)
  if err != nil {
    return
  }
  lines := strings.Split(string(b), "\n")
  for _, ln := range lines {
    ln = strings.TrimSpace(ln)
    if ln == "" || strings.HasPrefix(ln, "#") {
      continue
    }
    kv := strings.SplitN(ln, "=", 2)
    if len(kv) != 2 {
      continue
    }
    os.Setenv(strings.TrimSpace(kv[0]), strings.TrimSpace(kv[1]))
  }
}

func sftpConnect() (*sftp.Client, error) {
  host := envOr("SFTP_HOST", "")
  if host == "" {
    return nil, fmt.Errorf("SFTP_HOST requerido")
  }
  port := envOr("SFTP_PORT", "22")
  user := envOr("SFTP_USER", "")
  pass := envOr("SFTP_PASSWORD", "")
  if user == "" || pass == "" {
    return nil, fmt.Errorf("SFTP_USER/SFTP_PASSWORD requeridos")
  }

  hostKeyStr := envOr("SFTP_HOST_KEY", "")
  if hostKeyStr == "" {
    return nil, fmt.Errorf("SFTP_HOST_KEY requerido (formato OpenSSH authorized_keys: ssh-ed25519 AAAA... sftp-host)")
  }
  pubKey, _, _, _, err := ssh.ParseAuthorizedKey([]byte(hostKeyStr))
  if err != nil {
    return nil, fmt.Errorf("SFTP_HOST_KEY inválido: %w", err)
  }

  cfg := &ssh.ClientConfig{
    User:            user,
    Auth:            []ssh.AuthMethod{ssh.Password(pass)},
    HostKeyCallback: ssh.FixedHostKey(pubKey),
    Timeout:         20 * time.Second,
  }
  conn, err := ssh.Dial("tcp", host+":"+port, cfg)
  if err != nil {
    return nil, err
  }
  c, err := sftp.NewClient(conn)
  if err != nil {
    return nil, err
  }
  return c, nil
}

func ensureRemoteDir(s *sftp.Client, p string) error {
  // MkdirAll no falla si ya existe
  return s.MkdirAll(p)
}

func uploadFile(s *sftp.Client, localPath, remotePath string) error {
  in, err := os.Open(localPath)
  if err != nil {
    return err
  }
  defer in.Close()

  if err := ensureRemoteDir(s, filepath.ToSlash(filepath.Dir(remotePath))); err != nil {
    return err
  }

  out, err := s.Create(remotePath)
  if err != nil {
    return err
  }
  defer out.Close()

  _, err = io.Copy(out, in)
  return err
}

type Matched struct {
  SchemaVersion int `json:"schema_version"`
  Client struct {
    ID   int    `json:"id"`
    Code string `json:"code"`
  } `json:"client"`
  Audio struct {
    RecordID       string  `json:"record_id"`
    SourceFilename *string `json:"source_filename"`
    RawPath        *string `json:"raw_path"`
    WavPath        *string `json:"wav_path"`
  } `json:"audio"`
  Call struct {
    Tipo             int     `json:"tipo"`
    Telefono         string  `json:"telefono"`
    IDAgente         int     `json:"id_agente"`
    CIDLlamada       *string `json:"cid_llamada"`
    Anexo            *string `json:"anexo"`
    ParseOK          bool    `json:"parse_ok"`
    FechaGestion     string  `json:"fecha_gestion"`
    Hora             string  `json:"hora"`
    FechaGestionParse string `json:"fecha_gestion_parse"`
  } `json:"call"`
  Agent struct {
    NombreAgente *string `json:"nombre_agente"`
  } `json:"agent"`
  Probe struct {
    FfprobeOK   *bool    `json:"ffprobe_ok"`
    DurationSec *float64 `json:"duration_sec"`
  } `json:"probe"`
  Lookup struct {
    OK bool `json:"ok"`
  } `json:"lookup"`
  Rpta struct {
    DescripRpta string `json:"descrip_rpta"`
  } `json:"rpta"`
  Data map[string]any `json:"data"`
}

type UploadItem struct {
  RecordID string `json:"record_id"`
  JsonIn   string `json:"json_in"`
  JsonOut  string `json:"json_out,omitempty"`
  WavPath  string `json:"wav_path"`
  SendOK   bool   `json:"send_ok"`
  Reason   string `json:"reason,omitempty"`
}

type UploadReport struct {
  Client   string       `json:"client"`
  Date     string       `json:"date"`
  RunID    string       `json:"run_id"`
  DryRun   bool         `json:"dry_run"`
  Total    int          `json:"total"`
  Valid    int          `json:"valid"`
  Skipped  int          `json:"skipped"`
  Items    []UploadItem `json:"items"`
}

func asInt(v any) int {
	switch t := v.(type) {
	case float64:
		return int(t)
	case float32:
		return int(t)
	case int:
		return t
	case int32:
		return int(t)
	case int64:
		return int(t)
	case string:
		n, _ := strconv.Atoi(t)
		return n
	default:
		return 0
	}
}

func isPresent(v any) bool {
  if v == nil {
    return false
  }
  switch t := v.(type) {
  case string:
    return strings.TrimSpace(t) != ""
  default:
    // numbers/bools/objects considered present if not nil
    return true
  }
}

func getData(m map[string]any, key string) (any, bool) {
  v, ok := m[key]
  if !ok || v == nil {
    return nil, false
  }
  return v, true
}

func buildOutgoing(m *Matched) (map[string]any, []string, []string) {
  // returns: out, requiredKeys, optionalKeys
  out := map[string]any{}

  // generales
  out["cliente"] = m.Client.Code
  out["telefono"] = m.Call.Telefono
  out["id_agente"] = m.Call.IDAgente
  out["fecha_gestion"] = m.Call.FechaGestion
  out["hora"] = m.Call.Hora
  if m.Probe.DurationSec != nil {
    out["duration_sec"] = *m.Probe.DurationSec
  } else {
    out["duration_sec"] = nil
  }
  if m.Agent.NombreAgente != nil {
    out["nombre_agente"] = *m.Agent.NombreAgente
  } else {
    out["nombre_agente"] = nil
  }
  out["descrip_rpta"] = m.Rpta.DescripRpta

  // campos comunes que vienen del data (ambos clientes)
  if v, ok := getData(m.Data, "nombre_rpta"); ok { out["nombre_rpta"] = v } else { out["nombre_rpta"] = nil }
  if v, ok := getData(m.Data, "observacion"); ok { out["observacion"] = v } else { out["observacion"] = nil }

  required := []string{"cliente", "telefono", "id_agente", "fecha_gestion", "hora", "duration_sec", "nombre_agente", "descrip_rpta", "nombre_rpta", "observacion"}
  optional := []string{"fecha_compromiso", "monto_compromiso"}

  switch strings.ToLower(m.Client.Code) {
  case "natura":
    // EXACTAMENTE los campos del ejemplo
    // desde data
    keys := []string{"ciclo", "codigo_deudor", "deuda_total", "dias_atraso", "monto_campana", "monto_compromiso", "fecha_compromiso", "nombre_deudor"}
    for _, k := range keys {
      if v, ok := getData(m.Data, k); ok {
        out[k] = v
      } else {
        out[k] = nil
      }
    }
    required = append(required, "ciclo", "codigo_deudor", "deuda_total", "dias_atraso", "monto_campana", "nombre_deudor")
    // monto_compromiso y fecha_compromiso quedan opcionales (ya en optional)

	case "maf":
  	// generales + campos propios del cliente (data maf), pero normalizando compromiso a nombres estándar
	  mafKeys := []string{
  	  "n_cuota", "concatenado", "fec_vencimiento", "moneda", "monto_cuota",
    	"ultimo_tramo", "categoria", "placa", "documento", "deudor", "nid_opecodout",
	  }

  	for _, k := range mafKeys {
    	if v, ok := getData(m.Data, k); ok {
      	out[k] = v
	    } else {
  	    out[k] = nil
    	}
	  }

  	// Normalizar moneda: 1->SOLES, 2->DOLARES (requerido)
	  if v, ok := getData(m.Data, "moneda"); ok && v != nil {
  	  switch asInt(v) {
    	case 1:
      	out["moneda"] = "SOLES"
	    case 2:
  	    out["moneda"] = "DOLARES"
    	default:
      	out["moneda"] = nil // invalida (por ser requerido)
    	}
	  } else {
  	  out["moneda"] = nil // invalida (por ser requerido)
  	}

	  // compromiso (se permiten nulls)
  	if v, ok := getData(m.Data, "fec_comp"); ok {
    	out["fecha_compromiso"] = v
	  } else {
  	  out["fecha_compromiso"] = nil
  	}
  	if v, ok := getData(m.Data, "monto_comp_soles"); ok {
    	out["monto_compromiso"] = v
	  } else {
  	  out["monto_compromiso"] = nil
  	}

	  // dólares: si no viene, poner 0 para no invalidar (se sube como informativo)
  	if v, ok := getData(m.Data, "monto_comp_dolares"); ok && v != nil {
    	out["monto_compromiso_dolares"] = v
	  } else {
  	  out["monto_compromiso_dolares"] = 0.0
	  }

  	required = append(required, mafKeys...)
	  // fecha_compromiso y monto_compromiso opcionales por regla
  	required = append(required, "monto_compromiso_dolares")

  default:
    // por defecto: generales + todo data (excepto duplicados), sin renombrar.
    // Esto mantiene extensibilidad pero puede requerir ajuste por cliente.
    for k, v := range m.Data {
      if _, exists := out[k]; exists {
        continue
      }
      out[k] = v
      required = append(required, k)
    }
  }

  // asegurar order estable de required (sin duplicados)
  uniq := make(map[string]struct{}, len(required))
  outReq := make([]string, 0, len(required))
  for _, k := range required {
    if _, ok := uniq[k]; ok { continue }
    uniq[k] = struct{}{}
    outReq = append(outReq, k)
  }
  sort.Strings(outReq)

  return out, outReq, optional
}

func validateOutgoing(m *Matched, out map[string]any, required []string, optional []string) (bool, string) {
  if !m.Lookup.OK {
    return false, "lookup_ok=false"
  }
  if strings.TrimSpace(out["descrip_rpta"].(string)) == "" {
    return false, "descrip_rpta vacío"
  }
  if strings.TrimSpace(out["descrip_rpta"].(string)) == "OTRO" {
    return false, "descrip_rpta=OTRO"
  }

  opt := map[string]struct{}{}
  for _, k := range optional {
    opt[k] = struct{}{}
  }

  for _, k := range required {
    if _, isOpt := opt[k]; isOpt {
      continue
    }
    v, ok := out[k]
    if !ok || !isPresent(v) {
      return false, "campo requerido faltante/vacío: " + k
    }
  }

  // opcionales: se permite null, pero si viene string vacío, también inválido
  for _, k := range optional {
    v, ok := out[k]
    if !ok || v == nil {
      continue
    }
    if s, ok := v.(string); ok && strings.TrimSpace(s) == "" {
      return false, "campo opcional vacío: " + k
    }
  }

  // si existe ffprobe_ok y es false, invalidar (recomendado)
  if m.Probe.FfprobeOK != nil && !*m.Probe.FfprobeOK {
    return false, "ffprobe_ok=false"
  }

  return true, ""
}

func resolveWavFromJson(jsonPath, runDir, fallback string) string {
  b, err := os.ReadFile(jsonPath)
  if err != nil {
    return filepath.Join(runDir, "wav", fallback)
  }
  var v map[string]any
  if err := json.Unmarshal(b, &v); err != nil {
    return filepath.Join(runDir, "wav", fallback)
  }
  // intenta audio.wav_path relativo
  if audio, ok := v["audio"].(map[string]any); ok {
    if wp, ok := audio["wav_path"].(string); ok && strings.TrimSpace(wp) != "" {
      if filepath.IsAbs(wp) {
        return wp
      }
      return filepath.Join(runDir, filepath.FromSlash(wp))
    }
  }
  return filepath.Join(runDir, "wav", fallback)
}

func main() {
  var a Args
  flag.StringVar(&a.Client, "client", "", "client code (natura/maf)")
  flag.StringVar(&a.Date, "date", "", "yyyy-mm-dd")
  flag.StringVar(&a.SharedRoot, "shared-root", "/shared", "shared root")
  flag.StringVar(&a.RunID, "run-id", "", "run id")
  flag.BoolVar(&a.DryRun, "dry-run", false, "dry run (no sftp, pero genera manifest y prepared json)")
  flag.Parse()

  if a.Client == "" || a.Date == "" {
    fmt.Println("--client y --date requeridos")
    os.Exit(2)
  }
  if a.RunID == "" {
    a.RunID = time.Now().UTC().Format("20060102T150405Z")
  }

  // SFTP env solo se necesita si no es dry-run
  if !a.DryRun {
    readSecretEnv("/run/secrets/sftp-env")
  }

  runDir := filepath.Join(a.SharedRoot, "runs", a.Client, a.Date, a.RunID)
  matchedDir := filepath.Join(runDir, "matched")
  wavDir := filepath.Join(runDir, "wav")
  preparedDir := filepath.Join(runDir, "prepared", "json")
  manifestDir := filepath.Join(runDir, "manifests")

  jps, _ := filepath.Glob(filepath.Join(matchedDir, "*.json"))
  sort.Strings(jps)
  if len(jps) == 0 {
    log.Printf("no json en %s", matchedDir)
    os.Exit(0)
  }

  _ = os.MkdirAll(preparedDir, 0o755)
  _ = os.MkdirAll(manifestDir, 0o755)

  report := UploadReport{
    Client: a.Client, Date: a.Date, RunID: a.RunID, DryRun: a.DryRun,
    Total: len(jps),
    Items: make([]UploadItem, 0, len(jps)),
  }

  var s *sftp.Client
  var err error

  remoteBase := envOr("SFTP_REMOTE_BASE", "/incoming")
  remoteJson := filepath.ToSlash(filepath.Join(remoteBase, a.Client, a.Date, "json"))
  remoteAud := filepath.ToSlash(filepath.Join(remoteBase, a.Client, a.Date, "audios"))

  if !a.DryRun {
    s, err = sftpConnect()
    if err != nil {
      log.Fatalf("sftp connect: %v", err)
    }
    defer s.Close()
    if err := ensureRemoteDir(s, remoteJson); err != nil { log.Fatalf("mkdir %s: %v", remoteJson, err) }
    if err := ensureRemoteDir(s, remoteAud); err != nil { log.Fatalf("mkdir %s: %v", remoteAud, err) }
  }

  for _, jp := range jps {
    bn := filepath.Base(jp)
    wavName := strings.TrimSuffix(bn, ".json") + ".wav"
    wavPath := filepath.Join(wavDir, wavName)
    if _, err := os.Stat(wavPath); err != nil {
      wavPath = resolveWavFromJson(jp, runDir, wavName)
    }

    item := UploadItem{
      RecordID: strings.TrimSuffix(bn, ".json"),
      JsonIn:   filepath.ToSlash(filepath.Join("matched", bn)),
      WavPath:  filepath.ToSlash(filepath.Join("wav", filepath.Base(wavPath))),
      SendOK:   false,
    }

    b, err := os.ReadFile(jp)
    if err != nil {
      item.Reason = "read json: " + err.Error()
      report.Items = append(report.Items, item)
      report.Skipped++
      continue
    }

    var matched Matched
    if err := json.Unmarshal(b, &matched); err != nil {
      item.Reason = "parse json: " + err.Error()
      report.Items = append(report.Items, item)
      report.Skipped++
      continue
    }

    out, required, optional := buildOutgoing(&matched)
    ok, reason := validateOutgoing(&matched, out, required, optional)
    if !ok {
      item.Reason = reason
      report.Items = append(report.Items, item)
      report.Skipped++
      continue
    }

    // escribir prepared json (compacto)
    outBytes, err := json.Marshal(out)
    if err != nil {
      item.Reason = "marshal out: " + err.Error()
      report.Items = append(report.Items, item)
      report.Skipped++
      continue
    }

    outLocal := filepath.Join(preparedDir, bn)
    if err := os.WriteFile(outLocal, outBytes, 0o644); err != nil {
      item.Reason = "write prepared: " + err.Error()
      report.Items = append(report.Items, item)
      report.Skipped++
      continue
    }

    item.JsonOut = filepath.ToSlash(filepath.Join("prepared", "json", bn))
    item.SendOK = true
    report.Valid++

    if !a.DryRun {
      remoteJsonPath := filepath.ToSlash(filepath.Join(remoteJson, bn))
      remoteWavPath := filepath.ToSlash(filepath.Join(remoteAud, filepath.Base(wavPath)))

      if err := uploadFile(s, outLocal, remoteJsonPath); err != nil {
        item.SendOK = false
        item.Reason = "upload json: " + err.Error()
        report.Valid-- // revert
        report.Skipped++
        report.Items = append(report.Items, item)
        continue
      }
      if err := uploadFile(s, wavPath, remoteWavPath); err != nil {
        item.SendOK = false
        item.Reason = "upload wav: " + err.Error()
        report.Valid--
        report.Skipped++
        report.Items = append(report.Items, item)
        continue
      }
    }

    report.Items = append(report.Items, item)
  }

  // escribir manifest upload.json
  repBytes, _ := json.MarshalIndent(report, "", "  ")
  _ = os.WriteFile(filepath.Join(manifestDir, "upload.json"), repBytes, 0o644)

  log.Printf("upload summary total=%d valid=%d skipped=%d dry_run=%v", report.Total, report.Valid, report.Skipped, report.DryRun)
}
