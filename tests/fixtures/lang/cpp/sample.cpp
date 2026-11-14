class Widget {
public:
    int size() const {
        return 1;
    }
    int decl(int a);
};

template <typename T>
T top(T a) {
    return a;
}
