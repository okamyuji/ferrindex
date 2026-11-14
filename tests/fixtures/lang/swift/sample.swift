class Counter {
    var value = 0

    func bump() {
        value += 1
    }

    init() {
        value = 0
    }
}

protocol Runner {
    func go()
}

func top(a: Int) -> Int {
    return a + 1
}
