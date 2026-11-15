// DroidNode SPDY sidecar.
//
// The Rust kubelet terminates TLS and performs the HTTP "101 Switching
// Protocols" for `Upgrade: SPDY/3.1`. It then hands the raw upgraded
// connection to this helper over a Unix socket. This process speaks the
// Kubernetes streaming protocol (SPDY/3.1 + moby/spdystream) and bridges the
// streams to a proot process in the container rootfs.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"net"
	"net/http"
	"os"
	"os/exec"
	"strings"
	"sync"
	"syscall"
	"time"

	"github.com/moby/spdystream"
)

type metadata struct {
	Rootfs  string            `json:"rootfs"`
	Proot   string            `json:"proot"`
	Loader  string            `json:"loader"`
	TmpDir  string            `json:"tmpdir"`
	Command []string          `json:"command"`
	TTY     bool              `json:"tty"`
	Env     map[string]string `json:"env"`
	Mode    string            `json:"mode"`

	// A2 attach: one Unix socket per stream, created by the Rust agent.
	StdinPath  string `json:"stdin_path"`
	StdoutPath string `json:"stdout_path"`
	StderrPath string `json:"stderr_path"`
}

func main() {
	if len(os.Args) != 2 {
		fmt.Fprintln(os.Stderr, "usage: spdy-sidecar <unix-socket>")
		os.Exit(2)
	}
	conn, err := net.Dial("unix", os.Args[1])
	if err != nil {
		fmt.Fprintf(os.Stderr, "dial %s: %v\n", os.Args[1], err)
		os.Exit(1)
	}
	defer conn.Close()

	// First line is JSON metadata, then the SPDY byte stream begins.
	reader := bufio.NewReader(conn)
	line, err := reader.ReadString('\n')
	if err != nil {
		fmt.Fprintf(os.Stderr, "read metadata: %v\n", err)
		os.Exit(1)
	}
	var meta metadata
	if err := json.Unmarshal([]byte(line), &meta); err != nil {
		fmt.Fprintf(os.Stderr, "parse metadata: %v\n", err)
		os.Exit(1)
	}
	if len(meta.Command) == 0 && meta.Mode == "" {
		fmt.Fprintln(os.Stderr, "empty command")
		os.Exit(1)
	}

	switch meta.Mode {
	case "portforward":
		servePortForward(conn)
		return
	case "attach":
		serveAttach(conn, meta)
		return
	}

	cmd := exec.Command(meta.Proot, buildArgs(meta)...)
	cmd.Env = buildEnv(meta)
	cmd.SysProcAttr = &syscall.SysProcAttr{Setpgid: true}

	stdin, err := cmd.StdinPipe()
	if err != nil {
		exitWithError(conn, "stdin pipe: "+err.Error())
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		exitWithError(conn, "stdout pipe: "+err.Error())
	}
	stderr, err := cmd.StderrPipe()
	if err != nil {
		exitWithError(conn, "stderr pipe: "+err.Error())
	}
	if err := cmd.Start(); err != nil {
		exitWithError(conn, "start proot: "+err.Error())
	}

	spdyConn, err := spdystream.NewConnection(conn, true)
	if err != nil {
		_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
		fmt.Fprintf(os.Stderr, "spdy: %v\n", err)
		os.Exit(1)
	}

	var (
		mu         sync.Mutex
		errStream  *spdystream.Stream
		errReplied bool
		exitMsg    string
		exitSent   bool
		outWg      sync.WaitGroup
	)
	outputSeen := make(chan struct{})
	var outputOnce sync.Once
	markOutput := func() { outputOnce.Do(func() { close(outputSeen) }) }

	sendExit := func() {
		mu.Lock()
		msg := exitMsg
		s := errStream
		already := exitSent
		replied := errReplied
		mu.Unlock()
		if msg == "" || already || s == nil {
			return
		}
		if !replied {
			_ = s.SendReply(http.Header{}, false)
			mu.Lock()
			errReplied = true
			mu.Unlock()
		}
		_, _ = s.Write(failureStatus(msg))
		_ = s.Close()
		mu.Lock()
		exitSent = true
		mu.Unlock()
	}

	// One goroutine owns process reaping. It records the exit status, emits it
	// on the error channel if the client opened one, and closes the SPDY
	// connection so Serve() returns.
	go func() {
		waitErr := cmd.Wait()
		msg := ""
		if waitErr != nil {
			msg = "command terminated with exit code " + exitCode(waitErr)
		}
		mu.Lock()
		exitMsg = msg
		mu.Unlock()
		// Let stdout/stderr finish before publishing the failure status: kubectl
		// stops reading as soon as the error stream reports a non-zero exit.
		time.Sleep(100 * time.Millisecond)
		select {
		case <-outputSeen:
		case <-time.After(2 * time.Second):
		}
		// Ensure the stdout/stderr handler has registered in outWg.
		time.Sleep(50 * time.Millisecond)
		done := make(chan struct{})
		go func() {
			outWg.Wait()
			close(done)
		}()
		select {
		case <-done:
		case <-time.After(2 * time.Second):
		}
		sendExit()
		time.Sleep(100 * time.Millisecond)
		_ = spdyConn.Close()
	}()

	spdyConn.Serve(func(stream *spdystream.Stream) {
		typ := headerValue(stream.Headers(), "streamType")
		// Serve invokes handlers synchronously from its frame loop. Any blocking
		// read/write must run in a goroutine, otherwise one stream stalls the
		// whole SPDY connection (and Close deadlocks).
		switch typ {
		case "error":
			mu.Lock()
			errStream = stream
			mu.Unlock()
			go func() {
				mu.Lock()
				if !errReplied {
					_ = stream.SendReply(http.Header{}, false)
					errReplied = true
				}
				mu.Unlock()
				sendExit()
			}()
		case "stdin":
			go func() {
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(stdin, stream)
				_ = stdin.Close()
			}()
		case "stdout":
			markOutput()
			outWg.Add(1)
			go func() {
				defer outWg.Done()
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(stream, stdout)
				_ = stream.Close()
			}()
		case "stderr":
			markOutput()
			outWg.Add(1)
			go func() {
				defer outWg.Done()
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(stream, stderr)
				_ = stream.Close()
			}()
		case "resize":
			go func() {
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(io.Discard, stream)
				_ = stream.Close()
			}()
		default:
			go func() {
				_ = stream.SendReply(http.Header{"Error": []string{"unsupported stream type " + typ}}, false)
				_ = stream.Close()
			}()
		}
	})

	// Serve returned because the client closed the SPDY connection, or because
	// the process exited. Do not leave proot running.
	_ = syscall.Kill(-cmd.Process.Pid, syscall.SIGKILL)
}

// servePortForward implements the kubelet side of `kubectl port-forward`.
// The apiserver opens one SPDY stream per requested port with a `port` header;
// the stream is raw TCP data after SYN_REPLY.
func servePortForward(conn net.Conn) {
	spdyConn, err := spdystream.NewConnection(conn, true)
	if err != nil {
		fmt.Fprintf(os.Stderr, "portforward spdy: %v\n", err)
		return
	}
	spdyConn.Serve(func(stream *spdystream.Stream) {
		port := headerValue(stream.Headers(), "port")
		go func() {
			target, err := net.DialTimeout("tcp", "127.0.0.1:"+port, 5*time.Second)
			if err != nil {
				_ = stream.SendReply(http.Header{}, false)
				_, _ = stream.Write(failureStatus("port-forward dial failed: " + err.Error()))
				_ = stream.Close()
				return
			}
			defer target.Close()
			_ = stream.SendReply(http.Header{}, false)
			done := make(chan struct{}, 2)
			go func() {
				_, _ = io.Copy(target, stream)
				done <- struct{}{}
			}()
			go func() {
				_, _ = io.Copy(stream, target)
				done <- struct{}{}
			}()
			<-done
			_ = stream.Close()
		}()
	})
}

// serveAttach implements `kubectl attach`: Rust exposes three Unix sockets
// (container stdin/stdout/stderr) and this sidecar connects each SPDY stream
// to the matching socket.
func serveAttach(conn net.Conn, meta metadata) {
	spdyConn, err := spdystream.NewConnection(conn, true)
	if err != nil {
		fmt.Fprintf(os.Stderr, "attach spdy: %v\n", err)
		return
	}
	spdyConn.Serve(func(stream *spdystream.Stream) {
		typ := headerValue(stream.Headers(), "streamType")
		go func() {
			switch typ {
			case "stdin":
				c, err := net.Dial("unix", meta.StdinPath)
				if err != nil {
					_ = stream.SendReply(http.Header{}, false)
					_, _ = stream.Write(failureStatus("attach stdin: " + err.Error()))
					_ = stream.Close()
					return
				}
				defer c.Close()
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(c, stream)
			case "stdout":
				c, err := net.Dial("unix", meta.StdoutPath)
				if err != nil {
					_ = stream.SendReply(http.Header{}, false)
					_, _ = stream.Write(failureStatus("attach stdout: " + err.Error()))
					_ = stream.Close()
					return
				}
				defer c.Close()
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(stream, c)
				_ = stream.Close()
			case "stderr":
				c, err := net.Dial("unix", meta.StderrPath)
				if err != nil {
					_ = stream.SendReply(http.Header{}, false)
					_, _ = stream.Write(failureStatus("attach stderr: " + err.Error()))
					_ = stream.Close()
					return
				}
				defer c.Close()
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(stream, c)
				_ = stream.Close()
			case "resize":
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(io.Discard, stream)
				_ = stream.Close()
			case "error":
				_ = stream.SendReply(http.Header{}, false)
				_, _ = io.Copy(io.Discard, stream)
			default:
				_ = stream.SendReply(http.Header{}, false)
				_ = stream.Close()
			}
		}()
	})
}

func buildArgs(m metadata) []string {	args := []string{"-r", m.Rootfs, "-w", "/", "-0", "-b", "/dev", "-b", "/proc", "-b", "/sys"}
	if _, err := os.Stat("/etc/resolv.conf"); err == nil {
		if _, err := os.Stat(m.Rootfs + "/etc/resolv.conf"); err != nil {
			args = append(args, "-b", "/etc/resolv.conf")
		}
	}
	return append(args, m.Command...)
}

func buildEnv(m metadata) []string {
	env := []string{
		"PROOT_NO_SECCOMP=1",
		"PROOT_TMP_DIR=" + m.TmpDir,
		"TMPDIR=/tmp",
		"PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
	}
	if m.Loader != "" {
		env = append(env, "PROOT_LOADER="+m.Loader)
	}
	for k, v := range m.Env {
		env = append(env, k+"="+v)
	}
	// Keep inherited Android variables needed by the dynamic linker.
	for _, key := range []string{"LD_LIBRARY_PATH", "ANDROID_ROOT", "ANDROID_DATA"} {
		if v, ok := os.LookupEnv(key); ok {
			env = append(env, key+"="+v)
		}
	}
	return env
}

func headerValue(h http.Header, name string) string {
	for k, v := range h {
		if strings.EqualFold(k, name) && len(v) > 0 {
			return v[0]
		}
	}
	return ""
}

func exitCode(err error) string {
	if ee, ok := err.(*exec.ExitError); ok {
		if status, ok := ee.Sys().(syscall.WaitStatus); ok {
			return fmt.Sprintf("%d", status.ExitStatus())
		}
	}
	return "1"
}

// failureStatus is the JSON `metav1.Status` the apiserver expects on the error
// stream. Plain text makes remotecommand fail with "invalid character ...".
func failureStatus(msg string) []byte {
	code := "1"
	if i := strings.LastIndex(msg, " "); i >= 0 {
		code = msg[i+1:]
	}
	status := map[string]any{
		"kind":       "Status",
		"apiVersion": "v1",
		"status":     "Failure",
		"message":    msg,
		"reason":     "NonZeroExitCode",
		"details": map[string]any{
			"causes": []map[string]string{
				{"reason": "ExitCode", "message": code},
			},
		},
	}
	b, err := json.Marshal(status)
	if err != nil {
		return []byte(`{"kind":"Status","apiVersion":"v1","status":"Failure"}`)
	}
	return b
}

func exitWithError(conn net.Conn, msg string) {
	fmt.Fprintln(os.Stderr, msg)
	_ = conn.Close()
	os.Exit(1)
}
