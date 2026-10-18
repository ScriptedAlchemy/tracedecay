package priv

type sealed interface {
	mark()
}

type Ok struct{}

func (Ok) mark() {}
