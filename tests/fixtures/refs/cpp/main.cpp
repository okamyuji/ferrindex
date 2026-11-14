// target in a comment
void target() {}

struct Child : target {};

void caller() {
    const char *s = "target";
    target();
}
