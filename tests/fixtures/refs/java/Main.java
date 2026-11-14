// target in a comment
class Main {
    void target() {}
    class Child extends target {}
    void caller() {
        String s = "target";
        target();
    }
}
