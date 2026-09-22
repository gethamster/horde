// This deployment wrapper does not alter upstream runsc or OCI specs.
// Package beside byte-identical upstream runsc.real and its gvisor-bin helpers.
package main

import (
	"fmt"
	"os"
	"path/filepath"
	"syscall"
)

func wrappedArgs(binary string, original []string) []string {
	out := []string{binary, "--net-raw=true", "--allow-packet-socket-write=true"}
	return append(out, original...)
}

func main() {
	executable, err := os.Executable()
	if err != nil {
		fmt.Fprintln(os.Stderr, "cannot locate configured runsc wrapper")
		os.Exit(125)
	}
	real := filepath.Join(filepath.Dir(executable), "runsc.real")
	if err := syscall.Exec(real, wrappedArgs(real, os.Args[1:]), os.Environ()); err != nil {
		fmt.Fprintln(os.Stderr, "cannot execute configured upstream runsc:", err)
		os.Exit(125)
	}
}
