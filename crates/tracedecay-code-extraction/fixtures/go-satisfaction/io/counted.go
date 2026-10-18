package io

type Counted struct{}

func (Counted) Count() int {
	return 0
}

func (Counted) Read(p []byte) (int, error) {
	return 0, nil
}
