package demo;

public class Types {
    public enum Mode {
        A, B
    }

    public record Pair(int a, int b) {}

    public @interface Marker {}
}
