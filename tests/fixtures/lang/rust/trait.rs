trait Shape {
    fn area(&self) -> i32;
    fn name(&self) -> &str {
        "shape"
    }
}

extern "C" {
    fn ext(a: i32) -> i32;
}
