package shapes

import "example.com/sat/geom"

type Shape interface {
	Bounds() geom.Rect
	Name() string
}
