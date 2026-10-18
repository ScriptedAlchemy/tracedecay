package mathx

import "example.com/fixture/util"

func Mean(values []int) int {
	return Total(values) / len(values)
}

func Scale(value, factor int) int {
	return util.Clamp(value*factor, 0, 100)
}
