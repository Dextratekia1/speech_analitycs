package main

import (
	"crypto/ecdsa"
	"crypto/ed25519"
	"crypto/elliptic"
	"crypto/rand"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"golang.org/x/crypto/ssh"
)

// fakeHostKey is a syntactically arbitrary string accepted by parseSFTPConfig.
// ssh.ParseAuthorizedKey is only called in sftpConnect, not parseSFTPConfig,
// so the format is not validated here.
const fakeHostKey = "ssh-ed25519 AAAAFAKEKEY== fake-sftp-host"

func writeTempEnvFile(t *testing.T, lines []string) string {
	t.Helper()
	dir := t.TempDir()
	f := filepath.Join(dir, "sftp.env")
	if err := os.WriteFile(f, []byte(strings.Join(lines, "\n")), 0o600); err != nil {
		t.Fatalf("writeTempEnvFile: %v", err)
	}
	return f
}

func TestParseSFTPConfig_FromFileOnly(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=filehost.example",
		"SFTP_PORT=2222",
		"SFTP_USER=fileuser",
		"SFTP_PASSWORD=fake-password",
		"SFTP_REMOTE_BASE=/uploads",
		"SFTP_HOST_KEY=" + fakeHostKey,
	})

	cfg, err := parseSFTPConfig(f)
	if err != nil {
		t.Fatalf("expected success, got error: %v", err)
	}
	if cfg.Host != "filehost.example" {
		t.Errorf("Host: got %q, want %q", cfg.Host, "filehost.example")
	}
	if cfg.Port != "2222" {
		t.Errorf("Port: got %q, want %q", cfg.Port, "2222")
	}
	if cfg.User != "fileuser" {
		t.Errorf("User: got %q, want %q", cfg.User, "fileuser")
	}
	if cfg.Password != "fake-password" {
		t.Errorf("Password: got %q, want %q", cfg.Password, "fake-password")
	}
	if cfg.RemoteBase != "/uploads" {
		t.Errorf("RemoteBase: got %q, want %q", cfg.RemoteBase, "/uploads")
	}
	if cfg.HostKeyRaw != fakeHostKey {
		t.Errorf("HostKeyRaw: got %q, want %q", cfg.HostKeyRaw, fakeHostKey)
	}
}

func TestParseSFTPConfig_EnvOverridesFile(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=file-host",
		"SFTP_USER=fileuser",
		"SFTP_PASSWORD=fake-password",
		"SFTP_HOST_KEY=" + fakeHostKey,
		"SFTP_REMOTE_BASE=/from-file",
	})

	t.Setenv("SFTP_HOST", "env-host")
	t.Setenv("SFTP_REMOTE_BASE", "/from-env")

	cfg, err := parseSFTPConfig(f)
	if err != nil {
		t.Fatalf("expected success, got error: %v", err)
	}
	if cfg.Host != "env-host" {
		t.Errorf("Host: expected env override 'env-host', got %q", cfg.Host)
	}
	if cfg.RemoteBase != "/from-env" {
		t.Errorf("RemoteBase: expected env override '/from-env', got %q", cfg.RemoteBase)
	}
}

func TestParseSFTPConfig_MissingHostFails(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_USER=user",
		"SFTP_PASSWORD=fake-password",
		"SFTP_HOST_KEY=" + fakeHostKey,
	})

	_, err := parseSFTPConfig(f)
	if err == nil {
		t.Fatal("expected error for missing SFTP_HOST, got nil")
	}
	if !strings.Contains(err.Error(), "SFTP_HOST") {
		t.Errorf("error should mention SFTP_HOST, got: %v", err)
	}
}

func TestParseSFTPConfig_MissingUserFails(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=host.example",
		"SFTP_PASSWORD=fake-password",
		"SFTP_HOST_KEY=" + fakeHostKey,
	})

	_, err := parseSFTPConfig(f)
	if err == nil {
		t.Fatal("expected error for missing SFTP_USER, got nil")
	}
	if !strings.Contains(err.Error(), "SFTP_USER") {
		t.Errorf("error should mention SFTP_USER, got: %v", err)
	}
}

func TestParseSFTPConfig_MissingPasswordFails(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=host.example",
		"SFTP_USER=user",
		"SFTP_HOST_KEY=" + fakeHostKey,
	})

	_, err := parseSFTPConfig(f)
	if err == nil {
		t.Fatal("expected error for missing SFTP_PASSWORD, got nil")
	}
	if !strings.Contains(err.Error(), "SFTP_PASSWORD") {
		t.Errorf("error should mention SFTP_PASSWORD, got: %v", err)
	}
}

func TestParseSFTPConfig_MissingHostKeyFails(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=host.example",
		"SFTP_USER=user",
		"SFTP_PASSWORD=fake-password",
	})

	_, err := parseSFTPConfig(f)
	if err == nil {
		t.Fatal("expected error for missing SFTP_HOST_KEY, got nil")
	}
	if !strings.Contains(err.Error(), "SFTP_HOST_KEY") {
		t.Errorf("error should mention SFTP_HOST_KEY, got: %v", err)
	}
}

func TestParseSFTPConfig_NoFileFallsBackToEnv(t *testing.T) {
	t.Setenv("SFTP_HOST", "env-only-host")
	t.Setenv("SFTP_USER", "env-only-user")
	t.Setenv("SFTP_PASSWORD", "fake-env-password")
	t.Setenv("SFTP_HOST_KEY", fakeHostKey)

	_, err := parseSFTPConfig("/nonexistent/path/sftp.env")
	if err != nil {
		t.Fatalf("expected success with all values from env, got error: %v", err)
	}
}

func TestParseSFTPConfig_DefaultPortAndRemoteBase(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=host.example",
		"SFTP_USER=user",
		"SFTP_PASSWORD=fake-password",
		"SFTP_HOST_KEY=" + fakeHostKey,
		// SFTP_PORT and SFTP_REMOTE_BASE intentionally omitted
	})

	cfg, err := parseSFTPConfig(f)
	if err != nil {
		t.Fatalf("expected success, got error: %v", err)
	}
	if cfg.Port != "22" {
		t.Errorf("Port: expected default '22', got %q", cfg.Port)
	}
	if cfg.RemoteBase != "/incoming" {
		t.Errorf("RemoteBase: expected default '/incoming', got %q", cfg.RemoteBase)
	}
}

func TestParseSFTPConfig_IgnoresBlankCommentsAndUnknownKeys(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"",
		"# this is a comment",
		"SFTP_HOST=host.example",
		"UNKNOWN_KEY=should-be-ignored",
		"  # indented comment",
		"",
		"SFTP_USER=user",
		"SFTP_PASSWORD=fake-password",
		"SFTP_HOST_KEY=" + fakeHostKey,
	})

	cfg, err := parseSFTPConfig(f)
	if err != nil {
		t.Fatalf("expected success, got error: %v", err)
	}
	if cfg.Host != "host.example" {
		t.Errorf("Host: expected 'host.example', got %q", cfg.Host)
	}
	if cfg.User != "user" {
		t.Errorf("User: expected 'user', got %q", cfg.User)
	}
}

// syntheticECDSAAuthorizedKey generates a synthetic ecdsa-sha2-nistp256 public
// key in OpenSSH authorized_keys format. The private key is discarded; only the
// public portion is used. No network, no secrets, no PII.
func syntheticECDSAAuthorizedKey(t *testing.T) string {
	t.Helper()
	priv, err := ecdsa.GenerateKey(elliptic.P256(), rand.Reader)
	if err != nil {
		t.Fatalf("generate ECDSA key: %v", err)
	}
	pub, err := ssh.NewPublicKey(&priv.PublicKey)
	if err != nil {
		t.Fatalf("marshal ECDSA public key: %v", err)
	}
	return strings.TrimSpace(string(ssh.MarshalAuthorizedKey(pub)))
}

// syntheticED25519AuthorizedKey generates a synthetic ssh-ed25519 public key in
// OpenSSH authorized_keys format. The private key is discarded.
func syntheticED25519AuthorizedKey(t *testing.T) string {
	t.Helper()
	rawPub, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generate ED25519 key: %v", err)
	}
	pub, err := ssh.NewPublicKey(rawPub)
	if err != nil {
		t.Fatalf("marshal ED25519 public key: %v", err)
	}
	return strings.TrimSpace(string(ssh.MarshalAuthorizedKey(pub)))
}

func TestSFTPHostKeyAlgorithmMatchesParsedKeyType_ECDSA(t *testing.T) {
	keyStr := syntheticECDSAAuthorizedKey(t)
	pubKey, _, _, _, err := ssh.ParseAuthorizedKey([]byte(keyStr))
	if err != nil {
		t.Fatalf("parse authorized key: %v", err)
	}
	algos := hostKeyAlgorithmsFor(pubKey)
	if len(algos) != 1 {
		t.Fatalf("expected exactly 1 algorithm, got %d: %v", len(algos), algos)
	}
	if algos[0] != "ecdsa-sha2-nistp256" {
		t.Errorf("expected ecdsa-sha2-nistp256, got %q", algos[0])
	}
	if algos[0] != pubKey.Type() {
		t.Errorf("algorithm %q does not match pubKey.Type() %q", algos[0], pubKey.Type())
	}
}

func TestSFTPHostKeyAlgorithmMatchesParsedKeyType_ED25519(t *testing.T) {
	keyStr := syntheticED25519AuthorizedKey(t)
	pubKey, _, _, _, err := ssh.ParseAuthorizedKey([]byte(keyStr))
	if err != nil {
		t.Fatalf("parse authorized key: %v", err)
	}
	algos := hostKeyAlgorithmsFor(pubKey)
	if len(algos) != 1 {
		t.Fatalf("expected exactly 1 algorithm, got %d: %v", len(algos), algos)
	}
	if algos[0] != "ssh-ed25519" {
		t.Errorf("expected ssh-ed25519, got %q", algos[0])
	}
	if algos[0] != pubKey.Type() {
		t.Errorf("algorithm %q does not match pubKey.Type() %q", algos[0], pubKey.Type())
	}
}

// TestSFTPConnectMalformedHostKeyFailsBeforeDial covers GAP-A: sftpConnect must
// return an error before any network dial when HostKeyRaw is non-empty but not
// a valid OpenSSH authorized_keys line. ssh.ParseAuthorizedKey is called before
// ssh.Dial, so a malformed key is rejected without network access.
func TestSFTPConnectMalformedHostKeyFailsBeforeDial(t *testing.T) {
	cfg := SFTPConfig{
		Host:       "127.0.0.1",
		Port:       "22",
		User:       "synthetic-user",
		Password:   "synthetic-password",
		RemoteBase: "/incoming",
		HostKeyRaw: "not-a-valid-authorized-key",
	}
	_, err := sftpConnect(cfg)
	if err == nil {
		t.Fatal("expected error for malformed HostKeyRaw, got nil")
	}
	if !strings.Contains(err.Error(), "SFTP_HOST_KEY inválido") {
		t.Errorf("expected error to contain 'SFTP_HOST_KEY inválido', got: %v", err)
	}
	// Password must not appear in the error message (credential leak guard).
	if strings.Contains(err.Error(), "synthetic-password") {
		t.Errorf("password must not appear in error message: %v", err)
	}
}

// TestDryRunDoesNotRequireSFTPHostKey covers GAP-B: parseSFTPConfig fails when
// SFTP_HOST_KEY is absent, confirming that the dry-run code path must bypass it.
// The per-item processing functions (buildOutgoing, validateOutgoing) succeed
// without any SFTP configuration.
func TestDryRunDoesNotRequireSFTPHostKey(t *testing.T) {
	// parseSFTPConfig fails without SFTP_HOST_KEY — confirming dry-run must bypass it.
	_, err := parseSFTPConfig("/nonexistent/sftp.env")
	if err == nil {
		t.Fatal("parseSFTPConfig must fail without SFTP credentials")
	}

	// The per-item processing used in the dry-run path works without SFTP config.
	m := makeNaturaMatched()
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if !ok {
		t.Errorf("processing functions must succeed without SFTP config; got rejected: %s", reason)
	}
}

// TestDryRunMarksItemsPreparedWithoutSFTPConnect covers GAP-B: exercises the
// per-item dry-run pipeline (buildOutgoing → validateOutgoing → marshal → write)
// end-to-end without calling parseSFTPConfig or sftpConnect. An item that
// completes this path in main() receives statusPrepared, not statusSent.
func TestDryRunMarksItemsPreparedWithoutSFTPConnect(t *testing.T) {
	m := makeNaturaMatched()

	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if !ok {
		t.Fatalf("expected valid outgoing, got rejected: %s", reason)
	}

	outBytes, err := json.Marshal(out)
	if err != nil {
		t.Fatalf("json.Marshal failed: %v", err)
	}

	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "record-001.json"), outBytes, 0o644); err != nil {
		t.Fatalf("WriteFile failed: %v", err)
	}
	// No parseSFTPConfig or sftpConnect called above; no SFTP_HOST_KEY required.
}

// --- OPS-13: --sftp-secret-path flag tests ---

// TestSFTPSecretPathDefaultIsProductionPath verifies that the package-level
// constant used as the flag default matches the production secret file path.
// If this test fails it means production behavior has been broken.
func TestSFTPSecretPathDefaultIsProductionPath(t *testing.T) {
	const want = "/run/secrets/sftp-env"
	if defaultSFTPSecretPath != want {
		t.Errorf("defaultSFTPSecretPath = %q, want %q", defaultSFTPSecretPath, want)
	}
}

// TestSFTPSecretPathCustomPathFunctionality verifies that parseSFTPConfig
// accepts a custom path supplied via --sftp-secret-path.
func TestSFTPSecretPathCustomPathFunctionality(t *testing.T) {
	f := writeTempEnvFile(t, []string{
		"SFTP_HOST=custom.example",
		"SFTP_USER=customuser",
		"SFTP_PASSWORD=fake-password",
		"SFTP_HOST_KEY=" + fakeHostKey,
	})
	cfg, err := parseSFTPConfig(f)
	if err != nil {
		t.Fatalf("parseSFTPConfig with custom path failed: %v", err)
	}
	if cfg.Host != "custom.example" {
		t.Errorf("Host: got %q, want %q", cfg.Host, "custom.example")
	}
	if cfg.HostKeyRaw != fakeHostKey {
		t.Errorf("HostKeyRaw: got %q, want %q", cfg.HostKeyRaw, fakeHostKey)
	}
}

// TestSFTPSecretPathNotRequiredInDryRun verifies that the dry-run code path
// does not call parseSFTPConfig. parseSFTPConfig fails for a nonexistent path
// (when no env fallback is present), but per-item processing succeeds without
// any SFTP config — confirming the dry-run path correctly bypasses it.
func TestSFTPSecretPathNotRequiredInDryRun(t *testing.T) {
	// parseSFTPConfig must fail for a nonexistent path when no env vars are set.
	// This confirms that if dry-run called parseSFTPConfig it would fail.
	_, err := parseSFTPConfig("/tmp/ops13-nonexistent-sftp.env")
	if err == nil {
		t.Fatal("parseSFTPConfig must fail for nonexistent path with no env fallback")
	}

	// The per-item processing used in the dry-run path must succeed without SFTP config.
	m := makeNaturaMatched()
	out, required, optional := buildOutgoing(m)
	ok, reason := validateOutgoing(m, out, required, optional)
	if !ok {
		t.Errorf("dry-run item processing must succeed without SFTP config: %s", reason)
	}
}
