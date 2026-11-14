package main

// target in a comment
func target() {}

func caller() {
	_ = "target"
	target()
}
