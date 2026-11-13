// URL 短縮 API サーバ (Go 標準ライブラリのみ)。examples/shortener (Kasane) と同じ HTTP/1.1 の部分集合を話す。
//
//	POST /shorten (本文 = URL) -> 201 {"code":"1000","url":"..."} / 400 {"error":"invalid url"}
//	GET /<code>               -> 302 + Location / 404 {"error":"not found"}
//	GET /stats                -> 200 {"links":N}
//
// 1 接続 1 リクエスト (Connection: close)。既定は固定数のワーカー goroutine が接続を共有のキューから取り合う
// (Rust / Kasane と同じ形)。-mode spawn は接続ごとに goroutine を起こす (Go の慣用形)。
// 保存先はメモリのみ。使い方: server -addr 127.0.0.1:18080 -workers 4 [-mode pool|spawn]
package main

import (
	"bufio"
	"flag"
	"fmt"
	"io"
	"net"
	"os"
	"strconv"
	"strings"
	"sync"
)

const (
	maxHead = 8192
	maxBody = 4096
	offset  = 46656
)

var (
	mu    sync.Mutex
	store []string
)

func encode(n uint64) string {
	var digits []byte
	for {
		d := byte(n % 36)
		if d < 10 {
			digits = append(digits, '0'+d)
		} else {
			digits = append(digits, 'a'+d-10)
		}
		n /= 36
		if n == 0 {
			break
		}
	}
	for i, j := 0, len(digits)-1; i < j; i, j = i+1, j-1 {
		digits[i], digits[j] = digits[j], digits[i]
	}
	return string(digits)
}

func decode(text string) (uint64, bool) {
	if text == "" || len(text) > 12 || (len(text) > 1 && text[0] == '0') {
		return 0, false
	}
	var n uint64
	for _, c := range text {
		var d uint64
		switch {
		case c >= '0' && c <= '9':
			d = uint64(c - '0')
		case c >= 'a' && c <= 'z':
			d = uint64(c-'a') + 10
		default:
			return 0, false
		}
		n = n*36 + d
	}
	if n < offset {
		return 0, false
	}
	return n - offset, true
}

func validURL(url string) bool {
	if len(url) > 2048 {
		return false
	}
	rest := ""
	if strings.HasPrefix(url, "https://") {
		rest = url[len("https://"):]
	} else if strings.HasPrefix(url, "http://") {
		rest = url[len("http://"):]
	} else {
		return false
	}
	if rest == "" {
		return false
	}
	for _, c := range url {
		if c <= 32 || c == 127 {
			return false
		}
	}
	return true
}

func escape(sb *strings.Builder, text string) {
	for _, c := range text {
		switch {
		case c == '"':
			sb.WriteString("\\\"")
		case c == '\\':
			sb.WriteString("\\\\")
		case c < 32:
			fmt.Fprintf(sb, "\\u%04x", c)
		default:
			sb.WriteRune(c)
		}
	}
}

func linkBody(code, url string) string {
	var sb strings.Builder
	sb.WriteString("{\"code\":\"")
	sb.WriteString(code)
	sb.WriteString("\",\"url\":\"")
	escape(&sb, url)
	sb.WriteString("\"}")
	return sb.String()
}

func errorBody(message string) string {
	var sb strings.Builder
	sb.WriteString("{\"error\":\"")
	escape(&sb, message)
	sb.WriteString("\"}")
	return sb.String()
}

func reason(status int) string {
	switch status {
	case 200:
		return "OK"
	case 201:
		return "Created"
	case 302:
		return "Found"
	case 400:
		return "Bad Request"
	case 404:
		return "Not Found"
	case 405:
		return "Method Not Allowed"
	case 413:
		return "Payload Too Large"
	}
	return "Internal Server Error"
}

type response struct {
	status   int
	location string
	body     string
}

func route(method, path, body string) response {
	switch method {
	case "GET":
		if path == "/stats" {
			mu.Lock()
			n := len(store)
			mu.Unlock()
			return response{200, "", "{\"links\":" + strconv.Itoa(n) + "}"}
		}
		if path == "/shorten" {
			return response{405, "", errorBody("method not allowed")}
		}
		code := path[1:]
		if i, ok := decode(code); ok {
			mu.Lock()
			if i < uint64(len(store)) {
				url := store[i]
				mu.Unlock()
				return response{302, url, linkBody(code, url)}
			}
			mu.Unlock()
		}
		return response{404, "", errorBody("not found")}
	case "POST":
		if path == "/shorten" {
			url := strings.TrimSpace(body)
			if !validURL(url) {
				return response{400, "", errorBody("invalid url")}
			}
			mu.Lock()
			store = append(store, url)
			index := uint64(len(store) - 1)
			mu.Unlock()
			return response{201, "", linkBody(encode(index+offset), url)}
		}
	}
	return response{405, "", errorBody("method not allowed")}
}

// 要求を読む。status が 0 以外なら読めなかった理由 (400 / 413)。接続が途中で切れたときは -1。
func readRequest(r *bufio.Reader) (method, path, body string, status int) {
	head := make([]byte, 0, 512)
	for {
		b, err := r.ReadByte()
		if err != nil {
			return "", "", "", -1
		}
		head = append(head, b)
		if len(head) >= 4 && string(head[len(head)-4:]) == "\r\n\r\n" {
			break
		}
		if len(head) > maxHead {
			return "", "", "", 413
		}
	}
	lines := strings.Split(string(head[:len(head)-4]), "\r\n")
	parts := strings.Split(lines[0], " ")
	if len(parts) != 3 || (parts[2] != "HTTP/1.1" && parts[2] != "HTTP/1.0") || !strings.HasPrefix(parts[1], "/") {
		return "", "", "", 400
	}
	if parts[0] == "" || strings.ToUpper(parts[0]) != parts[0] {
		return "", "", "", 400
	}
	contentLength := 0
	for _, h := range lines[1:] {
		if name, value, ok := strings.Cut(h, ":"); ok && strings.EqualFold(name, "content-length") {
			n, err := strconv.Atoi(strings.TrimSpace(value))
			if err != nil || n < 0 {
				return "", "", "", 400
			}
			contentLength = n
		}
	}
	if contentLength > maxBody {
		return "", "", "", 413
	}
	buf := make([]byte, contentLength)
	if _, err := io.ReadFull(r, buf); err != nil {
		return "", "", "", -1
	}
	return parts[0], parts[1], string(buf), 0
}

func handle(c net.Conn) {
	defer c.Close()
	method, path, body, status := readRequest(bufio.NewReaderSize(c, 1024))
	var resp response
	switch status {
	case -1:
		return
	case 0:
		resp = route(method, path, body)
	case 413:
		resp = response{413, "", errorBody("payload too large")}
	default:
		resp = response{400, "", errorBody("bad request")}
	}
	var sb strings.Builder
	fmt.Fprintf(&sb, "HTTP/1.1 %d %s\r\nContent-Type: application/json\r\nContent-Length: %d\r\n", resp.status, reason(resp.status), len(resp.body))
	if resp.location != "" {
		sb.WriteString("Location: " + resp.location + "\r\n")
	}
	sb.WriteString("Connection: close\r\n\r\n")
	sb.WriteString(resp.body)
	io.WriteString(c, sb.String())
}

func main() {
	addr := flag.String("addr", "127.0.0.1:18080", "listen address")
	workers := flag.Int("workers", 4, "worker goroutines (pool mode)")
	mode := flag.String("mode", "pool", "pool | spawn")
	flag.Parse()
	l, err := net.Listen("tcp", *addr)
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	jobs := make(chan net.Conn, 64)
	if *mode == "pool" {
		for i := 0; i < *workers; i++ {
			go func() {
				for c := range jobs {
					handle(c)
				}
			}()
		}
	}
	for {
		c, err := l.Accept()
		if err != nil {
			continue
		}
		if *mode == "pool" {
			jobs <- c
		} else {
			go handle(c)
		}
	}
}
