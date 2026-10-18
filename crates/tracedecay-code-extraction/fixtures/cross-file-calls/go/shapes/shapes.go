package shapes

import (
	"example.com/fixture/mathx"
	"example.com/fixture/util"
)

func Area(width, height int) int {
	return util.Clamp(width, 0, 100) * height
}

func Perimeter(width, height int) int {
	return mathx.Total([]int{util.Clamp(width, 0, 100), height}) * 2
}
