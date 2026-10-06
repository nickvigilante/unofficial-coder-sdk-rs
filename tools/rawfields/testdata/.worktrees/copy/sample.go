package sample

import "encoding/json"

type Part struct {
	Args       json.RawMessage  `json:"args,omitempty"`
	Result     *json.RawMessage `json:"result"`
	Data       []byte           `json:"data"`
	Documented json.RawMessage  `json:"documented" swaggertype:"object"`
	Hidden     json.RawMessage  `json:"-"`
	IDs        []int64          `json:"ids"`
	Untagged   json.RawMessage
}
