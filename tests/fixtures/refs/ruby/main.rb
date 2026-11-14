# target in a comment
def target; end

class Child < target
end

def caller
  s = "target"
  target()
end
