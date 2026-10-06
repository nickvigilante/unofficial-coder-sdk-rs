package dup

import "encoding/json"

type T struct {
	R json.RawMessage `json:"r"`
}
