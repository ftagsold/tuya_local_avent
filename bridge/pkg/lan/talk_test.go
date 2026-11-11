package lan

import (
	"encoding/hex"
	"testing"
	"time"
)

// The bytes the Tuya Smart app sent, captured before encryption (padding cut).
func TestTalkMessagesMatchCapture(t *testing.T) {
	cases := []struct {
		name string
		got  []byte
		want string
	}{
		{"open seq 6", buildTalkControl(6, false), "785634120600000000000000080000000800000000000000" + "00000000"},
		{"close", buildTalkControl(0, true), "785634120000000000000000080000000800000000000000" + "01000000"},
	}
	for _, c := range cases {
		if h := hex.EncodeToString(c.got); h != c.want {
			t.Errorf("%s: got %s want %s", c.name, h, c.want)
		}
	}
	pcm := make([]byte, TalkFrameBytes)
	audio := buildTalkAudio(6, 0xcb59f082, pcm)
	wantHead := "78563412060000008100000003000000010000000000000082f059cb40010000"
	if h := hex.EncodeToString(audio[:talkHeaderLen]); h != wantHead {
		t.Errorf("audio header: got %s want %s", h, wantHead)
	}
	if len(audio) != talkHeaderLen+TalkFrameBytes {
		t.Errorf("audio length %d", len(audio))
	}
}

func TestTalkClockIsUnixMillisTruncated(t *testing.T) {
	// From the app capture: clock 3411669122 at unix ms 1794413031554.
	if got := talkClock(time.UnixMilli(1794413031554)); got != 3411669122 {
		t.Fatalf("talkClock = %d, want 3411669122", got)
	}
}
