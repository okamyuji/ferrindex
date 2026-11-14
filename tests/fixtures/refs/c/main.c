// target in a comment
void target(void) {}

void caller(void) {
    const char *s = "target";
    target();
}
