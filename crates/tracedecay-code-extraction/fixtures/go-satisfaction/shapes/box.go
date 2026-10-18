package shapes

import g "example.com/sat/geom"

type Box struct{}

func (*Box) Bounds() g.Rect {
	return g.Rect{}
}

func (Box) Name() string {
	return "box"
}
