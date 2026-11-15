// target in a comment
func target() {}

class Child: target {}

func caller() {
    let s = "target"
    target()
}
