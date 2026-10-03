package calc

type Wrong struct{}

func (Wrong) Add(a, b int64) int {
	return int(a + b)
}
