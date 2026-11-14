// target in a comment
function target() {}

class Child extends target {}

function caller() {
  const s = "target";
  target();
}
