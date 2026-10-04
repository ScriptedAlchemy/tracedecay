package io

import "io"

type Rows interface {
	io.Reader
	Count() int
}
