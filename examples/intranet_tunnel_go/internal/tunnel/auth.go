package tunnel

import (
	"crypto/hmac"
	"crypto/rand"
	"crypto/sha256"
	"encoding/binary"
	"fmt"
)

const (
	NonceLen      = 32
	TagLen        = 32
	AuthAlgorithm = "HMAC(SHA-256)"
)

var authContext = []byte("ciel-intranet-tunnel-auth-v1")

type AuthRejectedError struct {
	Message string
}

func (e AuthRejectedError) Error() string {
	return "server rejected handshake: " + e.Message
}

type HelloPayload struct {
	Version uint16
	Route   string
	Nonce   [NonceLen]byte
	Tag     [TagLen]byte
}

func EncodeHello(route string, psk []byte) ([]byte, error) {
	var nonce [NonceLen]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return nil, fmt.Errorf("crypto error: %w", err)
	}
	return EncodeHelloWithNonce(route, psk, nonce)
}

func EncodeHelloWithNonce(route string, psk []byte, nonce [NonceLen]byte) ([]byte, error) {
	if len(route) > 0xffff {
		return nil, fmt.Errorf("authentication error: route name too long")
	}
	tag := computeAuthTag(psk, Version, route, nonce)
	out := make([]byte, 2+2+len(route)+NonceLen+TagLen)
	binary.BigEndian.PutUint16(out[0:2], Version)
	binary.BigEndian.PutUint16(out[2:4], uint16(len(route)))
	copy(out[4:], []byte(route))
	offset := 4 + len(route)
	copy(out[offset:offset+NonceLen], nonce[:])
	copy(out[offset+NonceLen:], tag[:])
	return out, nil
}

func DecodeHello(input []byte) (HelloPayload, error) {
	if len(input) < 2+2+NonceLen+TagLen {
		return HelloPayload{}, fmt.Errorf("authentication error: malformed hello payload")
	}
	version := binary.BigEndian.Uint16(input[0:2])
	routeLen := int(binary.BigEndian.Uint16(input[2:4]))
	expected := 2 + 2 + routeLen + NonceLen + TagLen
	if len(input) != expected {
		return HelloPayload{}, fmt.Errorf("authentication error: malformed hello payload")
	}
	routeStart := 4
	routeEnd := routeStart + routeLen
	var hello HelloPayload
	hello.Version = version
	hello.Route = string(input[routeStart:routeEnd])
	copy(hello.Nonce[:], input[routeEnd:routeEnd+NonceLen])
	copy(hello.Tag[:], input[routeEnd+NonceLen:])
	return hello, nil
}

func VerifyHello(hello HelloPayload, expectedRoute string, psk []byte) error {
	if hello.Version != Version {
		return fmt.Errorf("authentication error: unsupported authentication version %d", hello.Version)
	}
	if hello.Route != expectedRoute {
		return fmt.Errorf("authentication error: route mismatch: expected %s, got %s", expectedRoute, hello.Route)
	}
	expected := computeAuthTag(psk, hello.Version, hello.Route, hello.Nonce)
	if !constantTimeEqual(expected[:], hello.Tag[:]) {
		return fmt.Errorf("authentication error: authentication tag mismatch")
	}
	return nil
}

func EncodeHelloOK() ([]byte, error) {
	var nonce [NonceLen]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return nil, fmt.Errorf("crypto error: %w", err)
	}
	out := make([]byte, 2+NonceLen)
	binary.BigEndian.PutUint16(out[0:2], Version)
	copy(out[2:], nonce[:])
	return out, nil
}

func DecodeHelloOK(input []byte) error {
	if len(input) != 2+NonceLen {
		return fmt.Errorf("protocol error: bad HelloOk length")
	}
	selected := binary.BigEndian.Uint16(input[0:2])
	if selected != Version {
		return fmt.Errorf("authentication error: unsupported authentication version %d", selected)
	}
	return nil
}

func computeAuthTag(psk []byte, version uint16, route string, nonce [NonceLen]byte) [TagLen]byte {
	mac := hmac.New(sha256.New, psk)
	mac.Write(authContext)
	var fixed [4]byte
	binary.BigEndian.PutUint16(fixed[0:2], version)
	binary.BigEndian.PutUint16(fixed[2:4], uint16(len(route)))
	mac.Write(fixed[:])
	mac.Write([]byte(route))
	mac.Write(nonce[:])
	sum := mac.Sum(nil)
	var out [TagLen]byte
	copy(out[:], sum)
	return out
}

func constantTimeEqual(left, right []byte) bool {
	maxLen := len(left)
	if len(right) > maxLen {
		maxLen = len(right)
	}
	diff := len(left) ^ len(right)
	for idx := 0; idx < maxLen; idx++ {
		var l, r byte
		if idx < len(left) {
			l = left[idx]
		}
		if idx < len(right) {
			r = right[idx]
		}
		diff |= int(l ^ r)
	}
	return diff == 0
}
