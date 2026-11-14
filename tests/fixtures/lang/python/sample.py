class Greeter:
    @staticmethod
    def hello(name):
        return "hi " + name

    def plain(self):
        return 1


@decorator
def top(a):
    return a
