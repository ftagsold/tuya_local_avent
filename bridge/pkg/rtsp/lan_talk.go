package rtsp

import (
	"encoding/binary"
	"sync"
	"time"

	"github.com/pion/rtp"

	"avent-webrtc-bridge/pkg/core"
	"avent-webrtc-bridge/pkg/lan"
)

// LAN talkback: RTSP backchannel audio (G.711 at 8 kHz, as the description
// offers it) becomes the monitor's talk messages (16 kHz PCM, see
// pkg/lan/talk.go). The talk opens on the first packet and closes once the
// client has been silent for lanTalkIdle, so a viewer that only watches never
// opens the speaker.

const lanTalkIdle = time.Second

// lanTalker is the part of lan.Client that talks.
type lanTalker interface {
	StartTalk() error
	SendTalk(pcm []byte) error
	StopTalk() error
}

type lanTalk struct {
	mu      sync.Mutex
	pending []byte // 16 kHz PCM not yet a whole frame
	last    int16  // previous 8 kHz sample, for interpolation
	idle    *time.Timer
	open    bool
	failed  bool // logged once per talk
}

// TalkPacket takes one backchannel RTP packet. Safe from any goroutine; a
// stopped source drops it.
func (s *LANSource) TalkPacket(p *rtp.Packet) {
	s.mu.Lock()
	client, stopped := s.client, s.stopped
	s.mu.Unlock()
	t, ok := client.(lanTalker)
	if stopped || !ok || p == nil || len(p.Payload) == 0 {
		return
	}
	var decode func(byte) int16
	switch p.PayloadType {
	case 0:
		decode = ulawToLinear
	case 8:
		decode = alawToLinear
	default:
		return
	}

	s.talk.mu.Lock()
	defer s.talk.mu.Unlock()
	if !s.talk.open {
		if err := t.StartTalk(); err != nil {
			s.talkFailed(err)
			return
		}
		s.talk.open, s.talk.failed = true, false
		core.Logger.Info().Msgf("Talkback to %s started (LAN)", s.deviceName)
	}
	// 8 kHz -> 16 kHz: each input sample yields the midpoint to the previous
	// one, then itself.
	for _, b := range p.Payload {
		x := decode(b)
		mid := int16((int32(s.talk.last) + int32(x)) / 2)
		s.talk.pending = binary.LittleEndian.AppendUint16(s.talk.pending, uint16(mid))
		s.talk.pending = binary.LittleEndian.AppendUint16(s.talk.pending, uint16(x))
		s.talk.last = x
	}
	whole := len(s.talk.pending) / lan.TalkFrameBytes * lan.TalkFrameBytes
	if whole > 0 {
		if err := t.SendTalk(s.talk.pending[:whole]); err != nil {
			s.talkFailed(err)
		}
		s.talk.pending = append(s.talk.pending[:0], s.talk.pending[whole:]...)
	}
	if s.talk.idle == nil {
		s.talk.idle = time.AfterFunc(lanTalkIdle, func() { s.endTalk(t) })
	} else {
		s.talk.idle.Reset(lanTalkIdle)
	}
}

func (s *LANSource) endTalk(t lanTalker) {
	s.talk.mu.Lock()
	defer s.talk.mu.Unlock()
	if !s.talk.open {
		return
	}
	s.talk.open, s.talk.pending, s.talk.last = false, s.talk.pending[:0], 0
	if err := t.StopTalk(); err != nil {
		core.Logger.Warn().Err(err).Msgf("Talkback to %s: stop", s.deviceName)
		return
	}
	core.Logger.Info().Msgf("Talkback to %s stopped", s.deviceName)
}

// talkFailed logs the first failure of a talk. Called with talk.mu held.
func (s *LANSource) talkFailed(err error) {
	if !s.talk.failed {
		s.talk.failed = true
		core.Logger.Warn().Err(err).Msgf("Talkback to %s failed", s.deviceName)
	}
}

// ulawToLinear decodes one G.711 µ-law byte.
func ulawToLinear(u byte) int16 {
	u = ^u
	t := (int32(u&0x0f) << 3) + 0x84
	t <<= (u & 0x70) >> 4
	if u&0x80 != 0 {
		return int16(0x84 - t)
	}
	return int16(t - 0x84)
}

// alawToLinear decodes one G.711 A-law byte.
func alawToLinear(a byte) int16 {
	a ^= 0x55
	t := int32(a&0x0f) << 4
	seg := (a & 0x70) >> 4
	switch seg {
	case 0:
		t += 8
	case 1:
		t += 0x108
	default:
		t += 0x108
		t <<= seg - 1
	}
	if a&0x80 != 0 {
		return int16(t)
	}
	return int16(-t)
}
