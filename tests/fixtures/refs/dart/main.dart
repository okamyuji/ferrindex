// target in a comment
void target() {}

class Child extends target {}

void caller() {
  var s = "target";
  target();
}
