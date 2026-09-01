package state

import "time"

type Clock interface {
	Now() time.Time
}
