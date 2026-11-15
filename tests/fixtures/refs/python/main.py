# target in a comment
def target(): pass

class Child(target):
    pass

def caller():
    s = "target"
    target()
