package tunnel

import (
	"errors"
	"fmt"
	"io"
	"net"
	"sync"
	"time"
)

type streamEvent struct {
	frame       Frame
	localClosed bool
	localFailed string
}

type controlWriter struct {
	mu   sync.Mutex
	conn net.Conn
}

func newControlWriter(conn net.Conn) *controlWriter {
	return &controlWriter{conn: conn}
}

func (writer *controlWriter) Send(frame Frame) error {
	writer.mu.Lock()
	defer writer.mu.Unlock()
	return WriteFrame(writer.conn, frame)
}

func (writer *controlWriter) Shutdown() {
	writer.mu.Lock()
	defer writer.mu.Unlock()
	_ = writer.conn.Close()
}

type streamRegistry struct {
	mu      sync.Mutex
	streams map[uint32]chan streamEvent
}

func newStreamRegistry() *streamRegistry {
	return &streamRegistry{streams: map[uint32]chan streamEvent{}}
}

func (registry *streamRegistry) Insert(streamID uint32, ch chan streamEvent) {
	registry.mu.Lock()
	defer registry.mu.Unlock()
	registry.streams[streamID] = ch
}

func (registry *streamRegistry) Get(streamID uint32) (chan streamEvent, bool) {
	registry.mu.Lock()
	defer registry.mu.Unlock()
	ch, ok := registry.streams[streamID]
	return ch, ok
}

func (registry *streamRegistry) Remove(streamID uint32) {
	registry.mu.Lock()
	defer registry.mu.Unlock()
	delete(registry.streams, streamID)
}

func spawnLocalReader(
	streamID uint32,
	reader net.Conn,
	writer *controlWriter,
	ch chan streamEvent,
	label string,
) {
	go func() {
		buffer := make([]byte, 16*1024)
		for {
			n, err := reader.Read(buffer)
			if n > 0 {
				payload := append([]byte(nil), buffer[:n]...)
				frame, buildErr := NewFrame(Data, streamID, payload)
				if buildErr != nil {
					notifyLocal(ch, streamEvent{localFailed: fmt.Sprintf("%s read path failed: %v", label, buildErr)})
					return
				}
				if err := writer.Send(frame); err != nil {
					notifyLocal(ch, streamEvent{localFailed: fmt.Sprintf("%s read path failed: %v", label, err)})
					return
				}
			}
			if err != nil {
				if isEOF(err) {
					notifyLocal(ch, streamEvent{localClosed: true})
				} else {
					notifyLocal(ch, streamEvent{localFailed: fmt.Sprintf("%s read error: %v", label, err)})
				}
				return
			}
		}
	}()
}

func notifyLocal(ch chan streamEvent, event streamEvent) {
	select {
	case ch <- event:
	case <-time.After(200 * time.Millisecond):
	}
}

func sendCloseWrite(writer *controlWriter, streamID uint32) error {
	frame, err := NewFrame(CloseWrite, streamID, nil)
	if err != nil {
		return err
	}
	return writer.Send(frame)
}

func sendCloseStream(writer *controlWriter, streamID uint32, code uint16, message string) error {
	payload, err := EncodeCloseReason(code, message)
	if err != nil {
		return err
	}
	frame, err := NewFrame(CloseStream, streamID, payload)
	if err != nil {
		return err
	}
	return writer.Send(frame)
}

func closeWrite(conn net.Conn) {
	type closeWriter interface {
		CloseWrite() error
	}
	if writable, ok := conn.(closeWriter); ok {
		_ = writable.CloseWrite()
	}
}

func drainBeforeClose(conn net.Conn) {
	closeWrite(conn)
	_ = conn.SetReadDeadline(time.Now().Add(200 * time.Millisecond))
	_, _ = io.Copy(io.Discard, conn)
}

func setNoDelay(conn net.Conn) {
	if tcp, ok := conn.(*net.TCPConn); ok {
		_ = tcp.SetNoDelay(true)
	}
}

func writeFull(conn net.Conn, data []byte) error {
	for len(data) > 0 {
		n, err := conn.Write(data)
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

func isEOF(err error) bool {
	return errors.Is(err, io.EOF) || err.Error() == "protocol error: unexpected eof"
}
