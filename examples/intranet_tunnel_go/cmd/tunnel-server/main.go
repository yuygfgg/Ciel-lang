package main

import (
	"fmt"
	"os"

	"intranet_tunnel_go/internal/tunnel"
)

func main() {
	config, err := tunnel.ParseServerArgs(os.Args[1:])
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		fmt.Fprintln(os.Stderr, tunnel.ServerUsage())
		os.Exit(2)
	}
	if err := tunnel.RunServer(config); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(2)
	}
}
