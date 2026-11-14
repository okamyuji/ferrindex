namespace Demo
{
    class Worker
    {
        public int Run(int x)
        {
            return x + 1;
        }
    }

    interface IRunner
    {
        void Go();
    }
}
