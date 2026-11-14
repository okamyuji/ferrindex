package sample

type Server struct {
	port int
}

func (s *Server) Start() int {
	return s.port
}

func Top(a int) int {
	return a + 1
}
