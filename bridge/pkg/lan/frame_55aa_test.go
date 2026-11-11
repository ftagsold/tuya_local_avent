package lan

import (
	"bytes"
	"encoding/binary"
	"hash/crc32"
	"testing"
)

var testKey = []byte("0123456789abcdef")

func TestPack55AARoundTrip(t *testing.T) {
	msg := []byte(`{"gwId":"abc","devId":"abc"}`)
	frame, err := Pack55AA(testKey, 1, CmdDPQuery, msg)
	if err != nil {
		t.Fatal(err)
	}
	if binary.BigEndian.Uint32(frame[0:4]) != Prefix55AA {
		t.Fatalf("prefix = %x", frame[0:4])
	}
	if binary.BigEndian.Uint32(frame[len(frame)-4:]) != Suffix55AA {
		t.Fatalf("suffix = %x", frame[len(frame)-4:])
	}
	h, plain, err := Unpack55AA(testKey, frame)
	if err != nil {
		t.Fatal(err)
	}
	if h.Cmd != CmdDPQuery {
		t.Fatalf("cmd = %d", h.Cmd)
	}
	if !bytes.Equal(plain, msg) {
		t.Fatalf("plain = %q", plain)
	}
}

// A device frame carries a 4-byte return code ahead of the ECB body. This
// mirrors the real DP_QUERY reply the monitor sends a 22-char-id device
// ("json obj data unvalid"), which the Rust port decodes identically.
func TestUnpack55AAStripsReturnCode(t *testing.T) {
	body, err := ecbEncrypt(testKey, []byte("json obj data unvalid"))
	if err != nil {
		t.Fatal(err)
	}
	payload := append([]byte{0, 0, 0, 1}, body...) // retcode 1
	frame := make([]byte, 16)
	binary.BigEndian.PutUint32(frame[0:4], Prefix55AA)
	binary.BigEndian.PutUint32(frame[4:8], 1)
	binary.BigEndian.PutUint32(frame[8:12], CmdDPQuery)
	binary.BigEndian.PutUint32(frame[12:16], uint32(len(payload)+8))
	frame = append(frame, payload...)
	frame = binary.BigEndian.AppendUint32(frame, crc32.ChecksumIEEE(frame))
	frame = binary.BigEndian.AppendUint32(frame, Suffix55AA)

	_, plain, err := Unpack55AA(testKey, frame)
	if err != nil {
		t.Fatal(err)
	}
	if string(plain) != "json obj data unvalid" {
		t.Fatalf("plain = %q", plain)
	}
}

func TestUnpack55AARejectsBadCRC(t *testing.T) {
	frame, err := Pack55AA(testKey, 1, CmdDPQuery, []byte(`{"x":1}`))
	if err != nil {
		t.Fatal(err)
	}
	frame[len(frame)-5] ^= 0xff // corrupt a payload byte
	if _, _, err := Unpack55AA(testKey, frame); err != ErrBadCRC {
		t.Fatalf("err = %v, want ErrBadCRC", err)
	}
}
