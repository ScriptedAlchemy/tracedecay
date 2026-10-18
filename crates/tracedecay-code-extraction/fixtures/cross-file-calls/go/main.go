package main

import (
	"fmt"

	"example.com/fixture/compat"
	"example.com/fixture/legacy"
	"example.com/fixture/mathx"
	"example.com/fixture/report"
	"example.com/fixture/shapes"
	"example.com/fixture/store"
)

func main() {
	s := &store.Store{}
	s.Add("Key", 1)
	s.Get("key")
	fmt.Println(report.Summary([]int{1, 2, 3}))
	fmt.Println(compat.Upgrade(" Text "))
	fmt.Println(shapes.Area(3, 4), shapes.Perimeter(3, 4))
	fmt.Println(legacy.OldFormat("x"), mathx.Scale(2, 3))
}
