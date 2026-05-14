// test-sftp-server — synthetic SFTP server for pipeline test-mode validation.
// Generates a fresh ed25519 host key on startup, writes a synthetic sftp.env
// to /run/sftp-setup/sftp.env (configurable via SFTP_TEST_ENV_PATH), then
// serves SFTP connections on 127.0.0.1:2222.
// All credentials are synthetic. No production secrets are used or required.
package main

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/binary"
	"fmt"
	"log"
	"net"
	"os"
	"path/filepath"
	"strings"

	"github.com/pkg/sftp"
	"golang.org/x/crypto/ssh"
)

const (
	defaultPort    = "2222"
	defaultUser    = "sftp-test-user"
	defaultPass    = "sftp-test-pass"
	defaultBase    = "/upload"
	defaultEnvPath = "/run/sftp-setup/sftp.env"
	listenAddr     = "127.0.0.1"
)

func envOr(key, def string) string {
	if v := os.Getenv(key); v != "" {
		return v
	}
	return def
}

func main() {
	port := envOr("SFTP_TEST_PORT", defaultPort)
	user := envOr("SFTP_TEST_USER", defaultUser)
	pass := envOr("SFTP_TEST_PASSWORD", defaultPass)
	base := envOr("SFTP_TEST_REMOTE_BASE", defaultBase)
	envPath := envOr("SFTP_TEST_ENV_PATH", defaultEnvPath)

	// Generate a fresh ephemeral ed25519 host key. The private key never leaves
	// this process; only the public key is written to the env file.
	_, priv, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		log.Fatalf("generate host key: %v", err)
	}
	signer, err := ssh.NewSignerFromKey(priv)
	if err != nil {
		log.Fatalf("new signer: %v", err)
	}
	pubKeyLine := strings.TrimSpace(string(ssh.MarshalAuthorizedKey(signer.PublicKey())))

	// Write the synthetic env file so callers can populate --test-sftp-env.
	if err := os.MkdirAll(filepath.Dir(envPath), 0o755); err != nil {
		log.Fatalf("mkdir %s: %v", filepath.Dir(envPath), err)
	}
	envContent := strings.Join([]string{
		"SFTP_HOST=" + listenAddr,
		"SFTP_PORT=" + port,
		"SFTP_USER=" + user,
		"SFTP_PASSWORD=" + pass,
		"SFTP_REMOTE_BASE=" + base,
		"SFTP_HOST_KEY=" + pubKeyLine,
	}, "\n") + "\n"
	if err := os.WriteFile(envPath, []byte(envContent), 0o600); err != nil {
		log.Fatalf("write env file %s: %v", envPath, err)
	}
	log.Printf("env file written: %s", envPath)

	// Configure SSH server with the generated host key.
	config := &ssh.ServerConfig{
		PasswordCallback: func(c ssh.ConnMetadata, pw []byte) (*ssh.Permissions, error) {
			if c.User() == user && string(pw) == pass {
				return nil, nil
			}
			return nil, fmt.Errorf("auth rejected")
		},
	}
	config.AddHostKey(signer)

	addr := listenAddr + ":" + port
	ln, err := net.Listen("tcp", addr)
	if err != nil {
		log.Fatalf("listen %s: %v", addr, err)
	}
	log.Printf("test-sftp-server listening on %s", addr)

	for {
		conn, err := ln.Accept()
		if err != nil {
			log.Printf("accept: %v", err)
			continue
		}
		go serve(conn, config)
	}
}

func serve(conn net.Conn, config *ssh.ServerConfig) {
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
		go serveSession(ch, requests)
	}
}

func serveSession(ch ssh.Channel, requests <-chan *ssh.Request) {
	defer ch.Close()
	for req := range requests {
		if req.Type == "subsystem" && len(req.Payload) >= 4 {
			nameLen := binary.BigEndian.Uint32(req.Payload)
			if uint32(len(req.Payload)) >= 4+nameLen &&
				string(req.Payload[4:4+nameLen]) == "sftp" {
				_ = req.Reply(true, nil)
				srv := sftp.NewRequestServer(ch, sftp.InMemHandler())
				_ = srv.Serve()
				return
			}
		}
		if req.WantReply {
			_ = req.Reply(false, nil)
		}
	}
}
