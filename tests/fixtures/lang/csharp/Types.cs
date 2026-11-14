namespace Demo
{
    public enum Color
    {
        Red,
    }

    public struct Point
    {
        public int X;
    }

    public record Person(string Name);

    public record struct Pair(int A, int B);

    public record class Plain
    {
        public int Id { get; set; }
    }
}
