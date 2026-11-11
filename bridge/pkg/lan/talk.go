package lan

import (
	"encoding/binary"
	"errors"
	"io"
	"sync"
	"time"
)

// Talkback over the LAN session: the monitor's speaker.
//
// Captured from the Tuya Smart app (libThingCameraSDK / libThingP2PSDK
// plaintext before AES): a talk is a control message that opens it, audio
// messages on the talk conversation (convTalk), and a control message that
// closes it.
//
//	open:  magic | seq | 0 | cmd 8 | len 8 | 0 | 0
//	close: magic | 0   | 0 | cmd 8 | len 8 | 0 | 1
//	audio: magic | seq | codec | rate | bits | channels | clock ms | len | PCM
//
// seq continues the request counter the startup messages began (0..5); the
// audio messages repeat the seq of the open that started them. The monitor
// answers an open with two replies on the control conversation (payload 1,
// then 3) and a close with one (payload 1). Audio is what the app sends:
// 320-byte frames of 16 kHz signed 16-bit little-endian mono PCM, about one
// every 10 ms, stamped with the wall clock in milliseconds, truncated to 32
// bits (the app's capture: clock 3411669122 at unix ms 1794413031554). A
// clock counted from the session start instead looks ~49 days stale to the
// monitor, which then stops streaming and restarts its service.
const (
	cmdTalk = 8

	// TalkSampleRate and TalkFrameBytes describe the PCM SendTalk expects.
	TalkSampleRate = 16000
	TalkFrameBytes = 320 // 10 ms at 16 kHz, 16-bit mono

	talkCodecPCM  = 0x81 // Thing_AV_CODEC_ID PCM
	talkRate16k   = 3    // index into sampleRates
	talkBits16    = 1
	talkMono      = 0
	talkHeaderLen = 32

	// firstTalkSeq follows the six startup messages (seq 0..5).
	firstTalkSeq = 6
)

// ErrNoSession is returned by the talk calls before Start finished the login.
var ErrNoSession = errors.New("lan: no session to talk on")

// talkState is the part of a Client that talks: control messages on the
// control conversation, audio on the talk conversation.
type talkState struct {
	mu      sync.Mutex
	control interface{ Write([]byte) (int, error) }
	// openAudio starts the talk conversation on first use; audio is its writer.
	openAudio func() (io.Writer, error)
	audio     io.Writer
	aesKey    []byte
	seq       uint32 // last request seq used
	open      bool
	talkSeq   uint32 // seq of the open talk
}

func buildTalkControl(seq uint32, closeTalk bool) []byte {
	msg := make([]byte, 28)
	binary.LittleEndian.PutUint32(msg[0:4], mediaMagic)
	binary.LittleEndian.PutUint32(msg[4:8], seq)
	binary.LittleEndian.PutUint32(msg[12:16], cmdTalk)
	binary.LittleEndian.PutUint32(msg[16:20], 8)
	if closeTalk {
		binary.LittleEndian.PutUint32(msg[24:28], 1)
	}
	return msg
}

func buildTalkAudio(seq, clockMs uint32, pcm []byte) []byte {
	msg := make([]byte, talkHeaderLen+len(pcm))
	binary.LittleEndian.PutUint32(msg[0:4], mediaMagic)
	binary.LittleEndian.PutUint32(msg[4:8], seq)
	binary.LittleEndian.PutUint32(msg[8:12], talkCodecPCM)
	binary.LittleEndian.PutUint32(msg[12:16], talkRate16k)
	binary.LittleEndian.PutUint32(msg[16:20], talkBits16)
	binary.LittleEndian.PutUint32(msg[20:24], talkMono)
	binary.LittleEndian.PutUint32(msg[24:28], clockMs)
	binary.LittleEndian.PutUint32(msg[28:32], uint32(len(pcm)))
	copy(msg[talkHeaderLen:], pcm)
	return msg
}

// attachTalk makes the control and talk conversations available once the
// login is through.
func (c *Client) attachTalk(control interface{ Write([]byte) (int, error) }, aesKey []byte, openAudio func() (io.Writer, error)) {
	c.talk.mu.Lock()
	defer c.talk.mu.Unlock()
	c.talk.control, c.talk.aesKey, c.talk.openAudio = control, aesKey, openAudio
	c.talk.seq = firstTalkSeq - 1
}

func (t *talkState) send(msg []byte) error {
	return t.sendOn(t.control, msg)
}

func (t *talkState) sendOn(w interface{ Write([]byte) (int, error) }, msg []byte) error {
	if w == nil {
		return ErrNoSession
	}
	sealed, err := seal(t.aesKey, msg)
	if err != nil {
		return err
	}
	_, err = w.Write(sealed)
	return err
}

// audioConv returns the talk conversation, opening it on first use.
func (t *talkState) audioConv() (io.Writer, error) {
	if t.audio == nil {
		if t.openAudio == nil {
			return nil, ErrNoSession
		}
		w, err := t.openAudio()
		if err != nil {
			return nil, err
		}
		t.audio = w
	}
	return t.audio, nil
}

// StartTalk opens the monitor's speaker. A talk already open is kept.
func (c *Client) StartTalk() error {
	t := &c.talk
	t.mu.Lock()
	defer t.mu.Unlock()
	if t.open {
		return nil
	}
	t.seq++
	if err := t.send(buildTalkControl(t.seq, false)); err != nil {
		return err
	}
	t.open, t.talkSeq = true, t.seq
	return nil
}

// SendTalk plays PCM (16 kHz, signed 16-bit little-endian, mono) on the
// monitor's speaker, opening the talk first if needed. It is cut into the
// app's 10 ms frames; a trailing partial frame is sent as it is.
func (c *Client) SendTalk(pcm []byte) error {
	if err := c.StartTalk(); err != nil {
		return err
	}
	t := &c.talk
	t.mu.Lock()
	defer t.mu.Unlock()
	w, err := t.audioConv()
	if err != nil {
		return err
	}
	for len(pcm) > 0 {
		n := min(TalkFrameBytes, len(pcm))
		clock := talkClock(time.Now())
		if err := t.sendOn(w, buildTalkAudio(t.talkSeq, clock, pcm[:n])); err != nil {
			return err
		}
		pcm = pcm[n:]
	}
	return nil
}

// talkClock is the audio timestamp the monitor expects: unix ms mod 2^32.
func talkClock(now time.Time) uint32 { return uint32(now.UnixMilli()) }

// StopTalk closes the monitor's speaker. A talk not open is a no-op.
func (c *Client) StopTalk() error {
	t := &c.talk
	t.mu.Lock()
	defer t.mu.Unlock()
	if !t.open {
		return nil
	}
	t.open = false
	return t.send(buildTalkControl(0, true))
}
