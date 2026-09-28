package compat

import (
	"example.com/fixture/legacy"
	"example.com/fixture/util"
)

func Shim(text string) string {
	return legacy.Normalize(text)
}

func Upgrade(text string) string {
	return util.Normalize(Shim(text))
}
