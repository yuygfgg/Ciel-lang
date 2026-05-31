package main

import (
	"fmt"
	"os"

	"intranet_tunnel_go/internal/tunnel"
)

func main() {
	config, err := tunnel.ParseAgentArgs(os.Args[1:])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		fmt.Fprintln(os.Stderr, tunnel.AgentUsage())
		os.Exit(2)
	}
	if err := tunnel.RunAgent(config); err != nil {
		fmt.Fprintln(os.Stderr, err)
		fmt.Fprintln(os.Stderr, tunnel.AgentUsage())
		os.Exit(2)
	}
}
