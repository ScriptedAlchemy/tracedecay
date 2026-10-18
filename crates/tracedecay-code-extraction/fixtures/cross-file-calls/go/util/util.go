package util

import "strings"

func Normalize(text string) string {
	return strings.ToLower(strings.TrimSpace(text))
}

func Clamp(value, low, high int) int {
	return max(low, min(value, high))
}
