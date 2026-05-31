package tunnel

import (
	"fmt"
	"net"
)

type ServerConfig struct {
	ControlAddr *net.TCPAddr
	PublicAddr  *net.TCPAddr
	Route       string
	PSK         []byte
}

type AgentConfig struct {
	ServerAddr *net.TCPAddr
	TargetAddr *net.TCPAddr
	Route      string
	PSK        []byte
}

func ServerUsage() string {
	return "usage: tunnel-server --control 127.0.0.1:7000 --public 127.0.0.1:7001 --route dev --psk secret"
}

func AgentUsage() string {
	return "usage: tunnel-agent --server 127.0.0.1:7000 --target 127.0.0.1:9000 --route dev --psk secret"
}

func ParseServerArgs(args []string) (ServerConfig, error) {
	values, err := parseFlagPairs(args)
	if err != nil {
		return ServerConfig{}, err
	}
	if err := rejectUnknown(values, map[string]bool{"--control": true, "--public": true, "--route": true, "--psk": true}); err != nil {
		return ServerConfig{}, err
	}
	controlText, err := required(values, "--control")
	if err != nil {
		return ServerConfig{}, err
	}
	control, err := parseTCPAddr(controlText)
	if err != nil {
		return ServerConfig{}, err
	}
	publicText, err := required(values, "--public")
	if err != nil {
		return ServerConfig{}, err
	}
	public, err := parseTCPAddr(publicText)
	if err != nil {
		return ServerConfig{}, err
	}
	route, err := required(values, "--route")
	if err != nil {
		return ServerConfig{}, err
	}
	psk, err := required(values, "--psk")
	if err != nil {
		return ServerConfig{}, err
	}
	return ServerConfig{
		ControlAddr: control,
		PublicAddr:  public,
		Route:       route,
		PSK:         []byte(psk),
	}, nil
}

func ParseAgentArgs(args []string) (AgentConfig, error) {
	values, err := parseFlagPairs(args)
	if err != nil {
		return AgentConfig{}, err
	}
	if err := rejectUnknown(values, map[string]bool{"--server": true, "--target": true, "--route": true, "--psk": true}); err != nil {
		return AgentConfig{}, err
	}
	serverText, err := required(values, "--server")
	if err != nil {
		return AgentConfig{}, err
	}
	server, err := parseTCPAddr(serverText)
	if err != nil {
		return AgentConfig{}, err
	}
	targetText, err := required(values, "--target")
	if err != nil {
		return AgentConfig{}, err
	}
	target, err := parseTCPAddr(targetText)
	if err != nil {
		return AgentConfig{}, err
	}
	route, err := required(values, "--route")
	if err != nil {
		return AgentConfig{}, err
	}
	psk, err := required(values, "--psk")
	if err != nil {
		return AgentConfig{}, err
	}
	return AgentConfig{
		ServerAddr: server,
		TargetAddr: target,
		Route:      route,
		PSK:        []byte(psk),
	}, nil
}

func parseFlagPairs(args []string) (map[string]string, error) {
	values := map[string]string{}
	for idx := 0; idx < len(args); idx += 2 {
		flag := args[idx]
		if len(flag) < 2 || flag[:2] != "--" {
			return nil, fmt.Errorf("expected flag, got %s", flag)
		}
		if idx+1 >= len(args) {
			return nil, fmt.Errorf("missing value for %s", flag)
		}
		value := args[idx+1]
		if len(value) >= 2 && value[:2] == "--" {
			return nil, fmt.Errorf("missing value for %s", flag)
		}
		if _, exists := values[flag]; exists {
			return nil, fmt.Errorf("duplicate flag %s", flag)
		}
		values[flag] = value
	}
	return values, nil
}

func rejectUnknown(values map[string]string, allowed map[string]bool) error {
	for key := range values {
		if !allowed[key] {
			return fmt.Errorf("unknown flag %s", key)
		}
	}
	return nil
}

func required(values map[string]string, flag string) (string, error) {
	value, ok := values[flag]
	if !ok {
		return "", fmt.Errorf("missing required flag %s", flag)
	}
	return value, nil
}

func parseTCPAddr(text string) (*net.TCPAddr, error) {
	if text == "" {
		return nil, fmt.Errorf("missing required flag")
	}
	addr, err := net.ResolveTCPAddr("tcp", text)
	if err != nil {
		return nil, fmt.Errorf("invalid socket address %s: %w", text, err)
	}
	return addr, nil
}
