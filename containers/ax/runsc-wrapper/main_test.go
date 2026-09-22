package main

import (
	"reflect"
	"testing"
)

func TestForwardAllArgumentsAndEnableDocumentedNetworkFlags(t *testing.T) {
	input := []string{"-root", "/private/state", "create", "-bundle", "/private/bundle", "guest"}
	got := wrappedArgs("/assets/runsc.real", input)
	want := []string{"/assets/runsc.real", "--net-raw=true", "--allow-packet-socket-write=true", "-root", "/private/state", "create", "-bundle", "/private/bundle", "guest"}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("got %#v, want %#v", got, want)
	}
	if input[0] != "-root" {
		t.Fatal("input arguments mutated")
	}
}

func TestKeepsVersionCommand(t *testing.T) {
	args := wrappedArgs("/assets/runsc.real", []string{"--version"})
	if args[len(args)-1] != "--version" {
		t.Fatal(args)
	}
}
