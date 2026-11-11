// Package lan speaks the Tuya LAN protocol on TCP 6668, which the monitor also
// uses to negotiate a video session when the client is on the same network.
//
// The framing here is deliberately independent of the cloud path in pkg/tuya:
// a LAN session is authenticated with the device's localKey rather than an
// account session, and it carries its own signalling in command 32.
package lan

import (
	"bytes"
	"crypto/aes"
	"crypto/cipher"
	"encoding/binary"
	"errors"
	"fmt"
	"hash/crc32"
)

// Frame prefixes. 55AA is the classic format used up to protocol 3.3; 6699
// carries AES-GCM and is what a 3.5 device speaks.
const (
	Prefix55AA = 0x000055AA
	Prefix6699 = 0x00006699
	Suffix6699 = 0x00009966
	Suffix55AA = 0x0000AA55

	header55AALen = 16
	header6699Len = 18
	suffixLen     = 4
	gcmIVLen      = 12
	gcmTagLen     = 16

	// maxFrameLen guards a desynced stream. It is far above the largest frame
	// we expect (the signalling offer is ~1.6 kB) on purpose: tinytuya's much
	// tighter ceiling silently swallows that offer.
	maxFrameLen = 65535
)

// Command numbers used on the LAN channel.
const (
	CmdSessKeyNegStart  = 3
	CmdSessKeyNegResp   = 4
	CmdSessKeyNegFinish = 5
	CmdHeartbeat        = 9
	CmdDPQuery          = 10
	// CmdIPCLan302 carries the WebRTC-style offer/answer exchange, the same
	// protocol-302 body the app publishes over cloud MQTT.
	CmdIPCLan302 = 32
)

// versionHeader33 is the 15-byte header a 3.3 device prepends to some
// commands. The commands this bridge sends on 3.3 (DP_QUERY, heartbeat and
// the 302 offer) do not carry it, so Pack55AA never adds it; Unpack55AA
// still strips it from device frames that do.
var versionHeader33 = []byte("3.3\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00")

var (
	ErrShortFrame = errors.New("lan: not enough data for a frame")
	ErrBadPrefix  = errors.New("lan: unrecognised frame prefix")
	ErrBadLength  = errors.New("lan: implausible frame length")
	ErrBadCRC     = errors.New("lan: bad CRC32")
)

// Header describes a framed message without decrypting it.
type Header struct {
	Prefix uint32
	Seq    uint32
	Cmd    uint32
	Length uint32
	Total  int
}

// ParseHeader reads a frame header from the front of buf.
func ParseHeader(buf []byte) (Header, error) {
	if len(buf) < 4 {
		return Header{}, ErrShortFrame
	}
	switch binary.BigEndian.Uint32(buf[:4]) {
	case Prefix6699:
		if len(buf) < header6699Len {
			return Header{}, ErrShortFrame
		}
		h := Header{
			Prefix: Prefix6699,
			Seq:    binary.BigEndian.Uint32(buf[6:10]),
			Cmd:    binary.BigEndian.Uint32(buf[10:14]),
			Length: binary.BigEndian.Uint32(buf[14:18]),
		}
		h.Total = int(h.Length) + header6699Len + suffixLen
		if h.Length == 0 || h.Length > maxFrameLen {
			return Header{}, ErrBadLength
		}
		return h, nil
	case Prefix55AA:
		if len(buf) < header55AALen {
			return Header{}, ErrShortFrame
		}
		h := Header{
			Prefix: Prefix55AA,
			Seq:    binary.BigEndian.Uint32(buf[4:8]),
			Cmd:    binary.BigEndian.Uint32(buf[8:12]),
			Length: binary.BigEndian.Uint32(buf[12:16]),
		}
		h.Total = int(h.Length) + header55AALen
		if h.Length == 0 || h.Length > maxFrameLen {
			return Header{}, ErrBadLength
		}
		return h, nil
	default:
		return Header{}, ErrBadPrefix
	}
}

// Pack6699 builds an AES-GCM frame.
//
// The iv is not a free choice: the monitor accepts a frame whose IV is random
// bytes, acknowledges it, and then silently ignores the payload. It only acts
// on frames whose IV is the ASCII text of a UUID, which is what the app sends.
// Callers pass a 12-byte ASCII UUID fragment.
func Pack6699(key, iv []byte, seq, cmd uint32, payload []byte) ([]byte, error) {
	if len(iv) != gcmIVLen {
		return nil, fmt.Errorf("lan: iv must be %d bytes, got %d", gcmIVLen, len(iv))
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	gcm, err := cipher.NewGCMWithNonceSize(block, gcmIVLen)
	if err != nil {
		return nil, err
	}

	// The length field covers the iv, the ciphertext and the tag, but not the
	// suffix that follows it — ParseHeader adds that back.
	length := uint32(gcmIVLen + len(payload) + gcmTagLen)
	head := make([]byte, header6699Len)
	binary.BigEndian.PutUint32(head[0:4], Prefix6699)
	binary.BigEndian.PutUint16(head[4:6], 0)
	binary.BigEndian.PutUint32(head[6:10], seq)
	binary.BigEndian.PutUint32(head[10:14], cmd)
	binary.BigEndian.PutUint32(head[14:18], length)

	// Everything after the prefix is authenticated but not encrypted.
	sealed := gcm.Seal(nil, iv, payload, head[4:])

	out := make([]byte, 0, header6699Len+len(iv)+len(sealed)+suffixLen)
	out = append(out, head...)
	out = append(out, iv...)
	out = append(out, sealed...)
	return binary.BigEndian.AppendUint32(out, Suffix6699), nil
}

// Unpack6699 decrypts a complete 6699 frame.
func Unpack6699(key, frame []byte) (Header, []byte, error) {
	h, err := ParseHeader(frame)
	if err != nil {
		return Header{}, nil, err
	}
	if h.Prefix != Prefix6699 {
		return h, nil, ErrBadPrefix
	}
	if len(frame) < h.Total {
		return h, nil, ErrShortFrame
	}
	body := frame[header6699Len : h.Total-suffixLen]
	if len(body) < gcmIVLen+gcmTagLen {
		return h, nil, ErrShortFrame
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return h, nil, err
	}
	gcm, err := cipher.NewGCMWithNonceSize(block, gcmIVLen)
	if err != nil {
		return h, nil, err
	}
	plain, err := gcm.Open(nil, body[:gcmIVLen], body[gcmIVLen:], frame[4:header6699Len])
	if err != nil {
		return h, nil, fmt.Errorf("lan: decrypt cmd %d: %w", h.Cmd, err)
	}
	return h, plain, nil
}

// StripRetcode removes the 4-byte return code that frames from the monitor
// carry and frames from the client do not. Getting this wrong shifts the
// session-key nonce and yields a key that decrypts nothing.
func StripRetcode(payload []byte) []byte {
	if len(payload) >= 4 && !bytes.HasPrefix(payload, []byte("{")) {
		return payload[4:]
	}
	return payload
}

// Pack55AA builds a classic protocol-3.3 frame: the payload is AES-128-ECB
// under the device localKey, CRC32-checked. No version header is added; the
// commands this bridge sends on 3.3 (DP_QUERY, heartbeat, the 302 offer) do
// not take one.
func Pack55AA(key []byte, seq, cmd uint32, payload []byte) ([]byte, error) {
	enc, err := ecbEncrypt(key, payload)
	if err != nil {
		return nil, err
	}
	// length covers the ciphertext plus the trailing crc(4) and suffix(4).
	length := uint32(len(enc) + suffixLen + 4)
	out := make([]byte, header55AALen)
	binary.BigEndian.PutUint32(out[0:4], Prefix55AA)
	binary.BigEndian.PutUint32(out[4:8], seq)
	binary.BigEndian.PutUint32(out[8:12], cmd)
	binary.BigEndian.PutUint32(out[12:16], length)
	out = append(out, enc...)
	out = binary.BigEndian.AppendUint32(out, crc32.ChecksumIEEE(out))
	return binary.BigEndian.AppendUint32(out, Suffix55AA), nil
}

// Unpack55AA decrypts a complete classic 3.3 frame. It strips the device
// return code and any version header, then AES-128-ECB decrypts the rest.
func Unpack55AA(key, frame []byte) (Header, []byte, error) {
	h, err := ParseHeader(frame)
	if err != nil {
		return Header{}, nil, err
	}
	if h.Prefix != Prefix55AA {
		return h, nil, ErrBadPrefix
	}
	if len(frame) < h.Total {
		return h, nil, ErrShortFrame
	}
	crcPos := h.Total - 8
	want := binary.BigEndian.Uint32(frame[crcPos : crcPos+4])
	if crc32.ChecksumIEEE(frame[:crcPos]) != want {
		return h, nil, ErrBadCRC
	}
	payload := frame[header55AALen:crcPos]
	// Device frames carry a 4-byte return code; strip it only when the rest
	// stays a whole number of AES blocks, optionally behind the 15-byte
	// version header, so a client frame's ciphertext is never cut.
	if len(payload) >= 4 && payload[0] == 0 && payload[1] == 0 && payload[2] == 0 {
		if rem := (len(payload) - 4) % 16; rem == 0 || rem == 15 {
			payload = payload[4:]
		}
	}
	if bytes.HasPrefix(payload, []byte("3.3")) && len(payload) >= len(versionHeader33) {
		payload = payload[len(versionHeader33):]
	}
	// Empty, or protocol-3.1 plaintext JSON (UDP 6666 discovery).
	if len(payload) == 0 || bytes.HasPrefix(payload, []byte("{")) {
		return h, payload, nil
	}
	plain, err := ecbDecrypt(key, payload)
	if err != nil {
		return h, nil, err
	}
	return h, plain, nil
}

func ecbEncrypt(key, data []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	bs := block.BlockSize()
	padded := pkcs7Pad(data, bs)
	out := make([]byte, len(padded))
	for i := 0; i < len(padded); i += bs {
		block.Encrypt(out[i:i+bs], padded[i:i+bs])
	}
	return out, nil
}

func ecbDecrypt(key, data []byte) ([]byte, error) {
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	bs := block.BlockSize()
	if len(data) == 0 || len(data)%bs != 0 {
		return nil, fmt.Errorf("lan: ECB input is %d bytes, not a block multiple", len(data))
	}
	out := make([]byte, len(data))
	for i := 0; i < len(data); i += bs {
		block.Decrypt(out[i:i+bs], data[i:i+bs])
	}
	return pkcs7Unpad(out, bs)
}

func pkcs7Pad(data []byte, bs int) []byte {
	n := bs - len(data)%bs
	out := make([]byte, len(data)+n)
	copy(out, data)
	for i := len(data); i < len(out); i++ {
		out[i] = byte(n)
	}
	return out
}

func pkcs7Unpad(data []byte, bs int) ([]byte, error) {
	if len(data) == 0 {
		return nil, errors.New("lan: empty ECB plaintext")
	}
	n := int(data[len(data)-1])
	if n == 0 || n > bs || n > len(data) {
		return nil, errors.New("lan: bad PKCS7 padding")
	}
	return data[:len(data)-n], nil
}
