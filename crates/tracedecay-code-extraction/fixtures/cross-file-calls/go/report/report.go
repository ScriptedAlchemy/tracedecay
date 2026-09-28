package report

import (
	"fmt"

	"example.com/fixture/mathx"
	u "example.com/fixture/util"
)

func FormatLine(value string) string {
	return u.Normalize(value) + "\n"
}

func Summary(values []int) string {
	return FormatLine(fmt.Sprint(mathx.Mean(values)))
}
