package rtsp

import (
	"testing"

	"github.com/pion/rtp"

	"avent-webrtc-bridge/pkg/lan"
)

func TestG711Decode(t *testing.T) {
	for _, c := range []struct {
		in   byte
		want int16
	}{{0xff, 0}, {0x7f, 0}, {0x00, -32124}, {0x80, 32124}} {
		if got := ulawToLinear(c.in); got != c.want {
			t.Errorf("ulaw %#x: got %d want %d", c.in, got, c.want)
		}
	}
	for _, c := range []struct {
		in   byte
		want int16
	}{{0xd5, 8}, {0x55, -8}, {0xaa, 32256}, {0x2a, -32256}} {
		if got := alawToLinear(c.in); got != c.want {
			t.Errorf("alaw %#x: got %d want %d", c.in, got, c.want)
		}
	}
}

type fakeTalker struct {
	starts, stops int
	sent          []byte
}

func (f *fakeTalker) StartTalk() error        { f.starts++; return nil }
func (f *fakeTalker) SendTalk(p []byte) error { f.sent = append(f.sent, p...); return nil }
func (f *fakeTalker) StopTalk() error         { f.stops++; return nil }

func TestTalkPacketFramesAt16k(t *testing.T) {
	f := &fakeTalker{}
	s := &LANSource{deviceName: "test"}
	s.client = struct {
		lanClient
		lanTalker
	}{nil, f}
	// 160 µ-law bytes = 20 ms at 8 kHz -> 640 bytes at 16 kHz = 2 frames.
	s.TalkPacket(&rtp.Packet{Header: rtp.Header{PayloadType: 0}, Payload: make([]byte, 160)})
	if f.starts != 1 || len(f.sent) != 2*lan.TalkFrameBytes {
		t.Fatalf("starts=%d sent=%d", f.starts, len(f.sent))
	}
	s.endTalk(f)
	if f.stops != 1 {
		t.Fatalf("stops=%d", f.stops)
	}
}
