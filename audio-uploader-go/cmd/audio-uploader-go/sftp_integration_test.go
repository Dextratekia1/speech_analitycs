//go:build integration

package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"io"
	"net"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"
)

// Synthetic credentials used only inside the test process. These are not real
// credentials and are never written to logs, error messages, or test output.
const (
	integTestUser     = "sftp-int-user"
	integTestPassword = "sftp-int-pass-synthetic"
)

// startTestSFTPServer starts an in-process SSH/SFTP server bound to a random
// localhost port. The server uses a freshly generated ed25519 host key and the
// synthetic credentials above. It serves an in-memory filesystem backed by
// sftp.InMemHandler(). The listener is closed via t.Cleanup when the test ends.
// Returns the bound port and the server's public key in OpenSSH authorized_keys
// format so callers can construct a matching sftp.env.
func startTestSFTPServer(t *testing.T) (port int, serverPubKeyLine string) {
	t.Helper()

	// Generate a synthetic server host key. The private key never leaves this function.
	_, serverPriv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generate server host key: %v", err)
	}
	signer, err := ssh.NewSignerFromKey(serverPriv)
	if err != nil {
		t.Fatalf("new signer from key: %v", err)
	}

	config := &ssh.ServerConfig{
		PasswordCallback: func(c ssh.ConnMetadata, pass []byte) (*ssh.Permissions, error) {
			if c.User() == integTestUser && string(pass) == integTestPassword {
				return nil, nil
			}
			return nil, fmt.Errorf("auth rejected")
		},
	}
	config.AddHostKey(signer)

	ln, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("listen on localhost: %v", err)
	}
	t.Cleanup(func() { ln.Close() })

	go integAcceptLoop(ln, config)

	port = ln.Addr().(*net.TCPAddr).Port
	serverPubKeyLine = strings.TrimSpace(string(ssh.MarshalAuthorizedKey(signer.PublicKey())))
	return port, serverPubKeyLine
}

// integAcceptLoop accepts connections and dispatches each in a goroutine.
// Exits when the listener is closed.
func integAcceptLoop(ln net.Listener, config *ssh.ServerConfig) {
	for {
		conn, err := ln.Accept()
		if err != nil {
			return
		}
		go integHandleSFTPConn(conn, config)
	}
}

// integHandleSFTPConn performs the SSH handshake and then handles session channels.
func integHandleSFTPConn(conn net.Conn, config *ssh.ServerConfig) {
	defer conn.Close()
	sshConn, chans, reqs, err := ssh.NewServerConn(conn, config)
	if err != nil {
		return
	}
	defer sshConn.Close()
	go ssh.DiscardRequests(reqs)
	for newChan := range chans {
		if newChan.ChannelType() != "session" {
			_ = newChan.Reject(ssh.UnknownChannelType, "unknown channel type")
			continue
		}
		ch, requests, err := newChan.Accept()
		if err != nil {
			return
		}
		go integDispatchSubsystem(ch, requests)
	}
}

// integDispatchSubsystem handles session channel requests and starts an SFTP
// subsystem server when the client requests it. The in-memory handler provides
// a virtual filesystem scoped to this session.
func integDispatchSubsystem(ch ssh.Channel, requests <-chan *ssh.Request) {
	defer ch.Close()
	for req := range requests {
		if req.Type == "subsystem" && len(req.Payload) >= 4 {
			nameLen := binary.BigEndian.Uint32(req.Payload)
			if uint32(len(req.Payload)) >= 4+nameLen {
				name := string(req.Payload[4 : 4+nameLen])
				if name == "sftp" {
					_ = req.Reply(true, nil)
					srv := sftp.NewRequestServer(ch, sftp.InMemHandler())
					_ = srv.Serve()
					return
				}
			}
		}
		if req.WantReply {
			_ = req.Reply(false, nil)
		}
	}
}

// writeSFTPEnv writes a synthetic sftp.env to t.TempDir() and returns its path.
// All values are synthetic. No real credentials, no production paths.
func writeSFTPEnv(t *testing.T, port int, pubKeyLine string) string {
	t.Helper()
	lines := []string{
		"SFTP_HOST=127.0.0.1",
		fmt.Sprintf("SFTP_PORT=%d", port),
		"SFTP_USER=" + integTestUser,
		"SFTP_PASSWORD=" + integTestPassword,
		"SFTP_REMOTE_BASE=/test-remote",
		"SFTP_HOST_KEY=" + pubKeyLine,
	}
	path := filepath.Join(t.TempDir(), "sftp.env")
	if err := os.WriteFile(path, []byte(strings.Join(lines, "\n")), 0o600); err != nil {
		t.Fatalf("write synthetic sftp.env: %v", err)
	}
	return path
}

// TestSFTPIntegration_UploadSucceedsWithCorrectHostKey exercises the full SFTP
// upload path through existing production functions:
//
//	parseSFTPConfig → sftpConnect → uploadFile → client.Open (read-back verification)
//
// The test server is in-process on localhost. All credentials and keys are
// synthetic. No production secrets are read. No external network is contacted.
func TestSFTPIntegration_UploadSucceedsWithCorrectHostKey(t *testing.T) {
	port, serverPubKeyLine := startTestSFTPServer(t)
	envPath := writeSFTPEnv(t, port, serverPubKeyLine)

	cfg, err := parseSFTPConfig(envPath)
	if err != nil {
		t.Fatalf("parseSFTPConfig: %v", err)
	}

	client, err := sftpConnect(cfg)
	if err != nil {
		t.Fatalf("sftpConnect: %v", err)
	}
	defer client.Close()

	// Write a synthetic local file with no PII.
	localDir := t.TempDir()
	localFile := filepath.Join(localDir, "synthetic-record.json")
	const syntheticContent = `{"test":"synthetic","client":"natura"}`
	if err := os.WriteFile(localFile, []byte(syntheticContent), 0o644); err != nil {
		t.Fatalf("write synthetic local file: %v", err)
	}

	remotePath := "/test-remote/natura/2026-01-08/json/synthetic-record.json"

	// uploadFile is the production upload function. It calls ensureRemoteDir
	// internally before writing the file.
	if err := uploadFile(client, localFile, remotePath); err != nil {
		t.Fatalf("uploadFile: %v", err)
	}

	// Read back via SFTP to verify the content was written correctly.
	rc, err := client.Open(remotePath)
	if err != nil {
		t.Fatalf("open uploaded file via SFTP: %v", err)
	}
	defer rc.Close()

	got, err := io.ReadAll(rc)
	if err != nil {
		t.Fatalf("read back uploaded file: %v", err)
	}
	if string(got) != syntheticContent {
		t.Errorf("uploaded content mismatch: got %q, want %q", string(got), syntheticContent)
	}
}

// TestSFTPIntegration_FailsClosedWithWrongHostKey verifies that sftpConnect fails
// closed when the pinned SFTP_HOST_KEY does not match the server's actual host key.
// ssh.FixedHostKey must reject the connection. ssh.InsecureIgnoreHostKey is not
// present anywhere in this test or in the production sftpConnect implementation.
func TestSFTPIntegration_FailsClosedWithWrongHostKey(t *testing.T) {
	port, _ := startTestSFTPServer(t)

	// Generate a different synthetic key — not the server's key.
	_, wrongPriv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatalf("generate wrong synthetic key: %v", err)
	}
	wrongSigner, err := ssh.NewSignerFromKey(wrongPriv)
	if err != nil {
		t.Fatalf("new signer for wrong key: %v", err)
	}
	wrongPubKeyLine := strings.TrimSpace(string(ssh.MarshalAuthorizedKey(wrongSigner.PublicKey())))

	envPath := writeSFTPEnv(t, port, wrongPubKeyLine)

	cfg, err := parseSFTPConfig(envPath)
	if err != nil {
		t.Fatalf("parseSFTPConfig with wrong key: %v", err)
	}

	_, err = sftpConnect(cfg)
	if err == nil {
		t.Fatal("sftpConnect must fail when SFTP_HOST_KEY does not match server key, but succeeded")
	}
	// The synthetic password must not appear in the error message (credential leak guard).
	if strings.Contains(err.Error(), integTestPassword) {
		t.Errorf("synthetic password must not appear in error message: %v", err)
	}
}

// TestSFTPIntegration_EnsureRemoteDirCreatesNestedPath verifies that
// ensureRemoteDir (which calls sftp.Client.MkdirAll) creates a multi-level
// remote path on the test server without error, and that the created directory
// is visible via Stat.
func TestSFTPIntegration_EnsureRemoteDirCreatesNestedPath(t *testing.T) {
	port, serverPubKeyLine := startTestSFTPServer(t)
	envPath := writeSFTPEnv(t, port, serverPubKeyLine)

	cfg, err := parseSFTPConfig(envPath)
	if err != nil {
		t.Fatalf("parseSFTPConfig: %v", err)
	}
	client, err := sftpConnect(cfg)
	if err != nil {
		t.Fatalf("sftpConnect: %v", err)
	}
	defer client.Close()

	dir := "/test-remote/natura/2026-05-13/audios"
	if err := ensureRemoteDir(client, dir); err != nil {
		t.Fatalf("ensureRemoteDir(%q): %v", dir, err)
	}

	fi, err := client.Stat(dir)
	if err != nil {
		t.Fatalf("stat of created remote dir %q: %v", dir, err)
	}
	if !fi.IsDir() {
		t.Errorf("expected %q to be a directory after ensureRemoteDir, got mode %v", dir, fi.Mode())
	}
}
