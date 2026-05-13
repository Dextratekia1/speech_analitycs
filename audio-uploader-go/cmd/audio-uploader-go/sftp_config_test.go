package main

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
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
