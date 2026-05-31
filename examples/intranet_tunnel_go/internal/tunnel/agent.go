package tunnel

import (
	"errors"
	"fmt"
	"net"
	"os"
	"time"
)

type agentControlState struct {
	writer     *controlWriter
	streams    *streamRegistry
	targetAddr *net.TCPAddr
	shutdown   bool
}

func RunAgent(config AgentConfig) error {
	backoff := 100 * time.Millisecond
	for {
		err := startAgent(config)
		var authErr AuthRejectedError
		if errors.As(err, &authErr) {
			return err
		}
		fmt.Fprintf(os.Stderr, "agent connection failed: %v; retrying in %s\n", err, backoff)
		time.Sleep(backoff)
		backoff *= 2
		if backoff > 2*time.Second {
			backoff = 2 * time.Second
		}
	}
}

func startAgent(config AgentConfig) error {
	conn, err := net.DialTCP("tcp", nil, config.ServerAddr)
	if err != nil {
		return err
	}
	setNoDelay(conn)
	if err := handleAgentHandshake(conn, config.Route, config.PSK); err != nil {
		_ = conn.Close()
		return err
	}
	fmt.Fprintf(os.Stderr, "agent connected to server %s\n", config.ServerAddr)
	fmt.Fprintln(os.Stderr, "tunnel-agent: authenticated")

	control := &agentControlState{
		writer:     newControlWriter(conn),
		streams:    newStreamRegistry(),
		targetAddr: config.TargetAddr,
	}
	return agentControlReader(conn, control)
}

func handleAgentHandshake(conn net.Conn, route string, psk []byte) error {
	payload, err := EncodeHello(route, psk)
	if err != nil {
		return err
	}
	hello, err := NewFrame(Hello, 0, payload)
	if err != nil {
		return err
	}
	if err := WriteFrame(conn, hello); err != nil {
		return err
	}

	response, err := ReadFrame(conn)
	if err != nil {
		return err
	}
	switch response.Kind {
	case HelloOK:
		return DecodeHelloOK(response.Payload)
	case ErrorFrame:
		message, _ := DecodeErrorMessage(response.Payload)
		return AuthRejectedError{Message: message}
	default:
		return fmt.Errorf("protocol error: expected HelloOk or Error")
	}
}

func agentControlReader(conn net.Conn, control *agentControlState) error {
	for {
		frame, err := ReadFrame(conn)
		if err != nil {
			control.writer.Shutdown()
			return err
		}
		switch frame.Kind {
		case OpenStream:
			if frame.StreamID == 0 || len(frame.Payload) != 0 {
				control.writer.Shutdown()
				return fmt.Errorf("protocol error: malformed OpenStream")
			}
			ch := make(chan streamEvent, 64)
			control.streams.Insert(frame.StreamID, ch)
			go agentStreamWorker(frame.StreamID, control, ch)
		case Data, CloseWrite, CloseStream:
			ch, ok := control.streams.Get(frame.StreamID)
			if !ok {
				if frame.Kind == Data {
					_ = sendCloseStream(control.writer, frame.StreamID, 20, "unknown stream")
				}
				continue
			}
			ch <- streamEvent{frame: frame}
		case Ping:
			if pong, err := NewFrame(Pong, 0, nil); err == nil {
				_ = control.writer.Send(pong)
			}
		case Pong:
		default:
			control.writer.Shutdown()
			return fmt.Errorf("protocol error: illegal frame kind on agent control connection")
		}
	}
}

func agentStreamWorker(streamID uint32, control *agentControlState, ch chan streamEvent) {
	target, err := net.DialTimeout("tcp", control.targetAddr.String(), 2*time.Second)
	if err != nil {
		message := fmt.Sprintf("target dial failed: %v", err)
		if payload, buildErr := EncodeOpenResultError(100, message); buildErr == nil {
			if frame, frameErr := NewFrame(OpenResult, streamID, payload); frameErr == nil {
				_ = control.writer.Send(frame)
			}
		}
		_ = sendCloseStream(control.writer, streamID, 100, message)
		control.streams.Remove(streamID)
		return
	}
	setNoDelay(target)
	defer func() {
		_ = target.Close()
		control.streams.Remove(streamID)
		fmt.Fprintf(os.Stderr, "agent stream %d closed\n", streamID)
	}()

	success, err := NewFrame(OpenResult, streamID, EncodeOpenResultSuccess())
	if err != nil || control.writer.Send(success) != nil {
		return
	}
	spawnLocalReader(streamID, target, control.writer, ch, "target")

	var remoteWriteClosed bool
	var localWriteClosed bool
	for {
		event := <-ch
		if event.localFailed != "" {
			_ = sendCloseStream(control.writer, streamID, 102, event.localFailed)
			return
		}
		if event.localClosed {
			localWriteClosed = true
			if err := sendCloseWrite(control.writer, streamID); err != nil {
				return
			}
			if remoteWriteClosed {
				return
			}
			continue
		}

		switch event.frame.Kind {
		case Data:
			if err := writeFull(target, event.frame.Payload); err != nil {
				_ = sendCloseStream(control.writer, streamID, 101, fmt.Sprintf("target write failed: %v", err))
				return
			}
		case CloseWrite:
			remoteWriteClosed = true
			closeWrite(target)
			if localWriteClosed {
				return
			}
		case CloseStream:
			return
		}
	}
}
