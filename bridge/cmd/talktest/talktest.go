// Package talktest checks LAN talkback end to end without a listener: it
// plays a 1 kHz tone on the monitor's speaker and looks for that tone in the
// monitor's own microphone stream.
package talktest

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"math"
	"os"
	"strconv"
	"sync"
	"time"

	"github.com/spf13/cobra"

	"avent-webrtc-bridge/pkg/lan"
)

type bridgeConfig struct {
	UID     string `json:"uid"`
	Cameras []struct {
		ID          string `json:"camera_id"`
		LocalKey    string `json:"local_key"`
		P2PPassword string `json:"p2p_password"`
		LanIP       string `json:"lan_ip"`
		LanProtocol string `json:"lan_protocol"`
	} `json:"cameras"`
}

func NewTalkTestCmd() *cobra.Command {
	var configPath string
	var seconds int
	var silent, zeros bool
	var audioDelay, talkConv int
	cmd := &cobra.Command{
		Use:   "talk-test",
		Short: "Play a 1 kHz tone on the monitor and detect it in its microphone",
		RunE: func(cmd *cobra.Command, _ []string) error {
			if silent {
				return runSilent(configPath)
			}
			amp := 0.6
			if zeros {
				amp = 0
			}
			return run(configPath, time.Duration(seconds)*time.Second, amp, time.Duration(audioDelay)*time.Millisecond, uint32(talkConv))
		},
	}
	cmd.Flags().StringVar(&configPath, "config", "bridge.json", "bridge config (the addon's)")
	cmd.Flags().IntVar(&seconds, "seconds", 3, "length of each phase")
	cmd.Flags().BoolVar(&silent, "silent", false, "open and close a talk without audio; report the monitor's control replies and mic frames")
	cmd.Flags().BoolVar(&zeros, "zeros", false, "send all-zero PCM instead of the tone: the talk path runs, nothing is audible")
	cmd.Flags().IntVar(&talkConv, "talk-conv", 0, "KCP conversation for talk audio (0 = the client default)")
	cmd.Flags().IntVar(&audioDelay, "audio-delay", 0, "milliseconds between opening the talk and the first audio frame")
	return cmd
}

// meter accumulates the share of 1 kHz in the incoming audio (Goertzel).
type meter struct {
	mu          sync.Mutex
	tone, total float64
	frames      int
}

func (m *meter) add(f *lan.AudioFrame) {
	if f.SampleRate == 0 || len(f.Samples) < 4 {
		return
	}
	n := len(f.Samples) / 2
	k := 2 * math.Cos(2*math.Pi*1000/float64(f.SampleRate))
	var s1, s2, energy float64
	for i := 0; i < n; i++ {
		x := float64(int16(binary.LittleEndian.Uint16(f.Samples[2*i:]))) / 32768
		energy += x * x
		s0 := x + k*s1 - s2
		s2, s1 = s1, s0
	}
	power := (s1*s1 + s2*s2 - k*s1*s2) / float64(n) * 2 / float64(n)
	m.mu.Lock()
	m.tone += power
	m.total += energy / float64(n)
	m.frames++
	m.mu.Unlock()
}

func (m *meter) take() (ratio float64, frames int) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if m.total > 0 {
		ratio = m.tone / m.total
	}
	frames = m.frames
	m.tone, m.total, m.frames = 0, 0, 0
	return
}

func run(path string, phase time.Duration, amp float64, audioDelay time.Duration, talkConv uint32) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var cfg bridgeConfig
	if err := json.Unmarshal(raw, &cfg); err != nil || len(cfg.Cameras) == 0 {
		return fmt.Errorf("config %s: no camera (%v)", path, err)
	}
	cam := cfg.Cameras[0]
	version, _ := strconv.ParseFloat(cam.LanProtocol, 64)
	m := &meter{}
	client := lan.NewClient(lan.ClientConfig{
		DeviceID: cam.ID, LocalKey: cam.LocalKey, Password: cam.P2PPassword,
		UID: cfg.UID, IP: cam.LanIP, Version: version, OnAudio: m.add,
		TalkConversation: talkConv,
		OnConversation:   func(conv uint32) { fmt.Printf("  conversation %d started\n", conv) },
	})
	defer client.Close()
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	if err := client.Start(ctx); err != nil {
		return fmt.Errorf("start: %w", err)
	}
	time.Sleep(2 * time.Second)
	m.take()

	time.Sleep(phase)
	quiet, qf := m.take()
	fmt.Printf("silent : 1 kHz share %.4f over %d frames\n", quiet, qf)

	if err := client.StartTalk(); err != nil {
		return fmt.Errorf("start talk: %w", err)
	}
	time.Sleep(audioDelay)
	frame := make([]byte, lan.TalkFrameBytes)
	samples := 0
	tick := time.NewTicker(10 * time.Millisecond)
	end := time.Now().Add(phase)
	for time.Now().Before(end) {
		<-tick.C
		for i := 0; i < len(frame)/2; i++ {
			v := int16(amp * 32767 * math.Sin(2*math.Pi*1000*float64(samples)/lan.TalkSampleRate))
			binary.LittleEndian.PutUint16(frame[2*i:], uint16(v))
			samples++
		}
		if err := client.SendTalk(frame); err != nil {
			return fmt.Errorf("send talk: %w", err)
		}
		if samples%lan.TalkSampleRate == 0 {
			m.mu.Lock()
			fmt.Printf("  talking %ds: %d mic frames so far\n", samples/lan.TalkSampleRate, m.frames)
			m.mu.Unlock()
		}
	}
	tick.Stop()
	loud, lf := m.take()
	if err := client.StopTalk(); err != nil {
		return fmt.Errorf("stop talk: %w", err)
	}
	fmt.Printf("talking: 1 kHz share %.4f over %d frames\n", loud, lf)
	if lf > 0 && loud > 10*quiet && loud > 0.05 {
		fmt.Println("RESULT: tone heard by the monitor's microphone -> talkback works")
	} else {
		fmt.Println("RESULT: tone not detected")
	}
	return nil
}

// runSilent opens a talk and closes it without sending audio: nothing plays.
// It reports the control replies (sizes) and incoming mic frames per phase.
func runSilent(path string) error {
	raw, err := os.ReadFile(path)
	if err != nil {
		return err
	}
	var cfg bridgeConfig
	if err := json.Unmarshal(raw, &cfg); err != nil || len(cfg.Cameras) == 0 {
		return fmt.Errorf("config %s: no camera (%v)", path, err)
	}
	cam := cfg.Cameras[0]
	version, _ := strconv.ParseFloat(cam.LanProtocol, 64)
	m := &meter{}
	start := time.Now()
	client := lan.NewClient(lan.ClientConfig{
		DeviceID: cam.ID, LocalKey: cam.LocalKey, Password: cam.P2PPassword,
		UID: cfg.UID, IP: cam.LanIP, Version: version, OnAudio: m.add,
		OnControl: func(n int) { fmt.Printf("  control reply %d bytes at +%dms\n", n, time.Since(start).Milliseconds()) },
	})
	defer client.Close()
	if err := client.Start(context.Background()); err != nil {
		return fmt.Errorf("start: %w", err)
	}
	time.Sleep(3 * time.Second)
	_, before := m.take()
	fmt.Printf("before talk: %d mic frames in 3s; opening talk at +%dms\n", before, time.Since(start).Milliseconds())
	if err := client.StartTalk(); err != nil {
		return err
	}
	time.Sleep(3 * time.Second)
	_, during := m.take()
	fmt.Printf("talk open  : %d mic frames in 3s; closing at +%dms\n", during, time.Since(start).Milliseconds())
	if err := client.StopTalk(); err != nil {
		return err
	}
	time.Sleep(3 * time.Second)
	_, after := m.take()
	fmt.Printf("after talk : %d mic frames in 3s\n", after)
	return nil
}
