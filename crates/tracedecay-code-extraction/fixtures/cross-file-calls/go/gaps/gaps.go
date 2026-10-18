package gaps

import (
	"strings"

	"example.com/fixture/missing"
	"example.com/fixture/util"
)

func Probe(text string) string {
	missing.Vanish(text)
	util.Absent(text)
	return strings.ToUpper(text)
}
