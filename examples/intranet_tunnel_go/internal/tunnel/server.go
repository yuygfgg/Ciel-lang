package tunnel

import (
	"fmt"
	"net"
	"os"
	"sync"
	"sync/atomic"
)

type serverControlState struct {
	writer   *controlWriter
	streams  *streamRegistry
	shutdown atomic.Bool
}

type serverRuntime struct {
	config        ServerConfig
	activeMu      sync.Mutex
	activeControl *serverControlState
	nextStreamID  atomic.Uint32
}

func RunServer(config ServerConfig) error {
	controlListener, err := net.ListenTCP("tcp", config.ControlAddr)
	if err != nil {
		return err
	}
	publicListener, err := net.ListenTCP("tcp", config.PublicAddr)
	if err != nil {
		_ = controlListener.Close()
		return err
	}

	runtime := &serverRuntime{config: config}
	runtime.nextStreamID.Store(1)
	go runtime.controlLoop(controlListener)
	go runtime.publicLoop(publicListener)

	fmt.Fprintf(os.Stderr, "server started: control=%s, public=%s\n", controlListener.Addr(), publicListener.Addr())
	fmt.Fprintln(os.Stderr, "tunnel-server: ready")
	select {}
}

func (runtime *serverRuntime) controlLoop(listener *net.TCPListener) {
	for {
		conn, err := listener.Accept()
		if err != nil {
			fmt.Fprintf(os.Stderr, "control listener error: %v\n", err)
			continue
		}
		setNoDelay(conn)
		fmt.Fprintf(os.Stderr, "control connection accepted from %s\n", conn.RemoteAddr())

		runtime.activeMu.Lock()
		hasActive := runtime.activeControl != nil
		runtime.activeMu.Unlock()
		if hasActive {
			fmt.Fprintln(os.Stderr, "rejecting additional control connection while agent is active")
			_ = conn.Close()
			continue
		}

		if err := handleServerHandshake(conn, runtime.config.Route, runtime.config.PSK); err != nil {
			fmt.Fprintf(os.Stderr, "authentication rejected: %v\n", err)
			if payload, buildErr := EncodeErrorMessage(err.Error()); buildErr == nil {
				if frame, frameErr := NewFrame(ErrorFrame, 0, payload); frameErr == nil {
					_ = WriteFrame(conn, frame)
				}
			}
			_ = conn.Close()
			continue
		}

		state := &serverControlState{
			writer:  newControlWriter(conn),
			streams: newStreamRegistry(),
		}
		runtime.activeMu.Lock()
		runtime.activeControl = state
		runtime.activeMu.Unlock()
		go runtime.serverControlReader(conn, state)
		fmt.Fprintf(os.Stderr, "authentication accepted for route %s\n", runtime.config.Route)
	}
}

func handleServerHandshake(conn net.Conn, route string, psk []byte) error {
	frame, err := ReadFrame(conn)
	if err != nil {
		return err
	}
	if frame.Kind != Hello {
		return fmt.Errorf("protocol error: expected Hello")
	}
	hello, err := DecodeHello(frame.Payload)
	if err != nil {
		return err
	}
	if err := VerifyHello(hello, route, psk); err != nil {
		return err
	}
	payload, err := EncodeHelloOK()
	if err != nil {
		return err
	}
	response, err := NewFrame(HelloOK, 0, payload)
	if err != nil {
		return err
	}
	return WriteFrame(conn, response)
}

func (runtime *serverRuntime) publicLoop(listener *net.TCPListener) {
	for {
		client, err := listener.Accept()
		if err != nil {
			fmt.Fprintf(os.Stderr, "public listener error: %v\n", err)
			continue
		}
		setNoDelay(client)

		runtime.activeMu.Lock()
		control := runtime.activeControl
		runtime.activeMu.Unlock()
		if control == nil {
			fmt.Fprintln(os.Stderr, "no authenticated agent available; closing public client")
			_ = client.Close()
			continue
		}

		streamID := runtime.nextStreamID.Add(1) - 1
		if streamID == 0 {
			streamID = runtime.nextStreamID.Add(1) - 1
		}
		ch := make(chan streamEvent, 64)
		control.streams.Insert(streamID, ch)
		open, err := NewFrame(OpenStream, streamID, nil)
		if err != nil {
			_ = client.Close()
			control.streams.Remove(streamID)
			continue
		}
		if err := control.writer.Send(open); err != nil {
			fmt.Fprintf(os.Stderr, "failed to send open stream: %v\n", err)
			_ = client.Close()
			control.streams.Remove(streamID)
			continue
		}
		go serverStreamWorker(streamID, client, control, ch)
	}
}

func (runtime *serverRuntime) serverControlReader(conn net.Conn, control *serverControlState) {
	defer func() {
		control.shutdown.Store(true)
		control.writer.Shutdown()
		runtime.activeMu.Lock()
		if runtime.activeControl == control {
			runtime.activeControl = nil
		}
		runtime.activeMu.Unlock()
	}()

	for {
		if control.shutdown.Load() {
			return
		}
		frame, err := ReadFrame(conn)
		if err != nil {
			fmt.Fprintf(os.Stderr, "server control read error: %v\n", err)
			return
		}
		switch frame.Kind {
		case OpenResult, Data, CloseWrite, CloseStream:
			ch, ok := control.streams.Get(frame.StreamID)
			if !ok {
				continue
			}
			ch <- streamEvent{frame: frame}
		case Ping:
			if pong, err := NewFrame(Pong, 0, nil); err == nil {
				_ = control.writer.Send(pong)
			}
		case Pong:
		default:
			fmt.Fprintln(os.Stderr, "protocol error: illegal frame kind on server control connection")
			return
		}
	}
}

func serverStreamWorker(streamID uint32, client net.Conn, control *serverControlState, ch chan streamEvent) {
	defer func() {
		_ = client.Close()
		control.streams.Remove(streamID)
		fmt.Fprintf(os.Stderr, "stream %d closed\n", streamID)
	}()

	var remoteWriteClosed bool
	var localWriteClosed bool
	var readerStarted bool

	for {
		event := <-ch
		if event.localFailed != "" {
			_ = sendCloseStream(control.writer, streamID, 3, event.localFailed)
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
		case OpenResult:
			if readerStarted {
				return
			}
			result, err := DecodeOpenResult(event.frame.Payload)
			if err != nil || !result.OK {
				drainBeforeClose(client)
				return
			}
			readerStarted = true
			spawnLocalReader(streamID, client, control.writer, ch, "client")
		case Data:
			if len(event.frame.Payload) > 0 {
				if err := writeFull(client, event.frame.Payload); err != nil {
					_ = sendCloseStream(control.writer, streamID, 2, err.Error())
					return
				}
			}
		case CloseWrite:
			remoteWriteClosed = true
			closeWrite(client)
			if localWriteClosed {
				return
			}
		case CloseStream:
			return
		}
	}
}
