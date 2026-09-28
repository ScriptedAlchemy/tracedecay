package legacy

import "strings"

func Normalize(text string) string {
	return strings.ToLower(text)
}

func OldFormat(value string) string {
	return "<" + Normalize(value) + ">"
}
