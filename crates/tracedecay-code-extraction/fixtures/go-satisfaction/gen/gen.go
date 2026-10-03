package gen

type Box[T any] interface {
	Get() T
}

type IntBox struct{}

func (IntBox) Get() int {
	return 0
}
