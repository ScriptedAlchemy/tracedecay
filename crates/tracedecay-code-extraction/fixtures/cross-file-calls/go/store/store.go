package store

import "example.com/fixture/util"

type Store struct {
	items map[string]int
}

func (s *Store) Add(key string, value int) {
	s.items[util.Normalize(key)] = value
}

func (s *Store) Get(key string) int {
	return s.items[util.Normalize(key)]
}
