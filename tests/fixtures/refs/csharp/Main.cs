// target in a comment
class Main {
    void target() {}
    class Child : target {}
    void caller() {
        var s = "target";
        target();
    }
}
