package tunnel

import (
	"encoding/binary"
	"fmt"
	"io"
)

const (
	Magic         uint32 = 0x4349544e
	Version       uint16 = 1
	HeaderLen            = 16
	MaxPayloadLen        = 65536
)

type FrameKind uint16

const (
	Hello       FrameKind = 1
	HelloOK     FrameKind = 2
	OpenStream  FrameKind = 3
	OpenResult  FrameKind = 4
	Data        FrameKind = 5
	CloseWrite  FrameKind = 6
	CloseStream FrameKind = 7
	Ping        FrameKind = 8
	Pong        FrameKind = 9
	ErrorFrame  FrameKind = 10
)

type Frame struct {
	Kind     FrameKind
	StreamID uint32
	Payload  []byte
}

type OpenResultPayload struct {
	OK      bool
	Code    uint16
	Message string
}

func NewFrame(kind FrameKind, streamID uint32, payload []byte) (Frame, error) {
	if err := validateStreamID(kind, streamID); err != nil {
		return Frame{}, err
	}
	if len(payload) > MaxPayloadLen {
		return Frame{}, fmt.Errorf("protocol error: payload length %d exceeds maximum", len(payload))
	}
	return Frame{Kind: kind, StreamID: streamID, Payload: payload}, nil
}

func ReadFrame(reader io.Reader) (Frame, error) {
	var header [HeaderLen]byte
	if _, err := io.ReadFull(reader, header[:]); err != nil {
		return Frame{}, mapReadError(err)
	}

	if magic := binary.BigEndian.Uint32(header[0:4]); magic != Magic {
		return Frame{}, fmt.Errorf("protocol error: bad magic 0x%08x", magic)
	}
	if version := binary.BigEndian.Uint16(header[4:6]); version != Version {
		return Frame{}, fmt.Errorf("protocol error: unsupported protocol version %d", version)
	}
	kind, err := parseFrameKind(binary.BigEndian.Uint16(header[6:8]))
	if err != nil {
		return Frame{}, err
	}
	streamID := binary.BigEndian.Uint32(header[8:12])
	length := binary.BigEndian.Uint32(header[12:16])
	if length > MaxPayloadLen {
		return Frame{}, fmt.Errorf("protocol error: payload length %d exceeds maximum", length)
	}
	if err := validateStreamID(kind, streamID); err != nil {
		return Frame{}, err
	}

	payload := make([]byte, int(length))
	if _, err := io.ReadFull(reader, payload); err != nil {
		return Frame{}, mapReadError(err)
	}
	return Frame{Kind: kind, StreamID: streamID, Payload: payload}, nil
}

func WriteFrame(writer io.Writer, frame Frame) error {
	if err := validateStreamID(frame.Kind, frame.StreamID); err != nil {
		return err
	}
	if len(frame.Payload) > MaxPayloadLen {
		return fmt.Errorf("protocol error: payload length %d exceeds maximum", len(frame.Payload))
	}

	var header [HeaderLen]byte
	binary.BigEndian.PutUint32(header[0:4], Magic)
	binary.BigEndian.PutUint16(header[4:6], Version)
	binary.BigEndian.PutUint16(header[6:8], uint16(frame.Kind))
	binary.BigEndian.PutUint32(header[8:12], frame.StreamID)
	binary.BigEndian.PutUint32(header[12:16], uint32(len(frame.Payload)))
	if err := writeAll(writer, header[:]); err != nil {
		return err
	}
	if len(frame.Payload) > 0 {
		if err := writeAll(writer, frame.Payload); err != nil {
			return err
		}
	}
	return nil
}

func EncodeOpenResultSuccess() []byte {
	return []byte{0}
}

func EncodeOpenResultError(code uint16, message string) ([]byte, error) {
	msg := []byte(message)
	if len(msg) > 0xffff {
		return nil, fmt.Errorf("protocol error: open result message too long")
	}
	out := make([]byte, 5+len(msg))
	out[0] = 1
	binary.BigEndian.PutUint16(out[1:3], code)
	binary.BigEndian.PutUint16(out[3:5], uint16(len(msg)))
	copy(out[5:], msg)
	return out, nil
}

func DecodeOpenResult(input []byte) (OpenResultPayload, error) {
	if len(input) == 0 {
		return OpenResultPayload{}, fmt.Errorf("protocol error: empty open result")
	}
	switch input[0] {
	case 0:
		if len(input) != 1 {
			return OpenResultPayload{}, fmt.Errorf("protocol error: success open result has trailing bytes")
		}
		return OpenResultPayload{OK: true}, nil
	case 1:
		if len(input) < 5 {
			return OpenResultPayload{}, fmt.Errorf("protocol error: short error open result")
		}
		code := binary.BigEndian.Uint16(input[1:3])
		length := int(binary.BigEndian.Uint16(input[3:5]))
		if len(input) != 5+length {
			return OpenResultPayload{}, fmt.Errorf("protocol error: open result length mismatch")
		}
		return OpenResultPayload{OK: false, Code: code, Message: string(input[5:])}, nil
	default:
		return OpenResultPayload{}, fmt.Errorf("protocol error: unknown open result status")
	}
}

func EncodeCloseReason(code uint16, message string) ([]byte, error) {
	msg := []byte(message)
	if len(msg) > 0xffff {
		return nil, fmt.Errorf("protocol error: close message too long")
	}
	out := make([]byte, 4+len(msg))
	binary.BigEndian.PutUint16(out[0:2], code)
	binary.BigEndian.PutUint16(out[2:4], uint16(len(msg)))
	copy(out[4:], msg)
	return out, nil
}

func EncodeErrorMessage(message string) ([]byte, error) {
	if len(message) > MaxPayloadLen {
		return nil, fmt.Errorf("protocol error: payload length %d exceeds maximum", len(message))
	}
	return []byte(message), nil
}

func DecodeErrorMessage(input []byte) (string, error) {
	return string(input), nil
}

func parseFrameKind(value uint16) (FrameKind, error) {
	switch FrameKind(value) {
	case Hello, HelloOK, OpenStream, OpenResult, Data, CloseWrite, CloseStream, Ping, Pong, ErrorFrame:
		return FrameKind(value), nil
	default:
		return 0, fmt.Errorf("protocol error: unknown frame kind %d", value)
	}
}

func (kind FrameKind) requiresZeroStream() bool {
	switch kind {
	case Hello, HelloOK, Ping, Pong, ErrorFrame:
		return true
	default:
		return false
	}
}

func validateStreamID(kind FrameKind, streamID uint32) error {
	if kind.requiresZeroStream() {
		if streamID == 0 {
			return nil
		}
		return fmt.Errorf("protocol error: invalid stream id %d for frame kind %d", streamID, kind)
	}
	if streamID != 0 {
		return nil
	}
	return fmt.Errorf("protocol error: invalid stream id 0 for frame kind %d", kind)
}

func mapReadError(err error) error {
	if err == io.EOF || err == io.ErrUnexpectedEOF {
		return fmt.Errorf("protocol error: unexpected eof")
	}
	return err
}

func writeAll(writer io.Writer, data []byte) error {
	for len(data) > 0 {
		n, err := writer.Write(data)
		if err != nil {
			return err
		}
		if n == 0 {
			return io.ErrShortWrite
		}
		data = data[n:]
	}
	return nil
}
